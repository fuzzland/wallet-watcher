use {
    crate::{
        balance_changes::{BalanceChange, BalanceChanges},
        config::{WalletWithContext, NATIVE_TOKEN},
        contract::{ERC20, WETH9},
        utils::{is_weth9, primitive_log_decode, U256AsDecimalStr},
    },
    alloy::{
        network::ReceiptResponse,
        primitives::{Address, TxHash, I256, U256},
        rpc::types::{
            trace::geth::{CallConfig, CallFrame, GethDebugBuiltInTracerType, GethDebugTracingOptions},
            AnyTransactionReceipt, Header,
        },
    },
    alloy_chains::Chain,
    eyre::{eyre, Context, ContextCompat},
    serde::{Deserialize, Serialize},
    serde_with::serde_as,
    std::collections::{HashSet, VecDeque},
    tracing::{info_span, instrument, trace},
};

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PnlReport {
    #[serde_as(as = "Vec<TxAndPositionAsStr>")]
    pub txs: Vec<TxAndPosition>,

    pub pnl: I256,

    #[serde(default, skip_serializing_if = "U256::is_zero")]
    #[serde_as(as = "U256AsDecimalStr")]
    pub builder_reward: U256,

    #[serde(default, skip_serializing_if = "U256::is_zero")]
    #[serde_as(as = "U256AsDecimalStr")]
    pub validator_bribe: U256,

    #[serde(default, skip_serializing_if = "BalanceChange::is_empty")]
    pub token_changes: BalanceChange,
}

impl PnlReport {
    pub fn tx_formatter(&self) -> PnlReportTxFormatWrapper {
        PnlReportTxFormatWrapper(self)
    }
}

pub fn process_block(
    chain: Chain,
    header: &Header,
    receipt_and_traces: &[(AnyTransactionReceipt, CallFrame)],
    wallets: &[WalletWithContext],
) -> eyre::Result<Vec<Option<PnlReport>>> {
    let mut reports = Vec::with_capacity(wallets.len());

    let mut balance_changes_all = Vec::with_capacity(receipt_and_traces.len());

    let all_involved_wallets = wallets
        .iter()
        .flat_map(|w| w.involved_wallets().iter())
        .chain(find_all_receipients(receipt_and_traces.iter().map(|(r, _)| r), wallets).iter())
        .cloned()
        .collect::<HashSet<_>>();

    for (i, (receipt, call_trace)) in receipt_and_traces.iter().enumerate() {
        let bcs = generate_pnl(chain, receipt, call_trace, None)
            .with_context(|| format!("Failed to generate balance changes for tx at index {i}"))?;

        let filtered_bcs = clone_and_retain_accounts(&bcs, &all_involved_wallets);

        balance_changes_all.push(BalanceChangesCache {
            filtered: filtered_bcs,
            full: bcs,
        });
    }

    for wallet in wallets {
        let s = info_span!("by_wallet", wallet = %wallet.address);
        let _g = s.enter();

        let is_builder =
            chain == Chain::mainnet() && wallet.builder.as_ref().map(|b| header.miner.eq(b)).unwrap_or_default();

        let (builder_reward, validator_bribe) = if is_builder {
            let reward = calculate_builder_reward(
                header.base_fee_per_gas.expect("Base fee per gas is not set").into(),
                receipt_and_traces.iter().map(|(r, _)| r),
            );

            let bribe = find_validator_bribe(&balance_changes_all);

            trace!(builder_reward = ?reward, validate_bribe = %bribe);

            (reward, bribe)
        } else {
            (U256::ZERO, U256::ZERO)
        };

        let all_involved_txs = balance_changes_all
            .iter()
            .enumerate()
            .filter_map(|(i, bc)| {
                let involved = bc.filtered.keys().any(|w| wallet.involved_wallets().contains(w));
                (involved && !is_shitcoin_airdrop(&bc.full)).then_some((receipt_and_traces[i].0.clone(), bc))
            })
            .collect::<Vec<_>>();

        if all_involved_txs.is_empty() && builder_reward.is_zero() {
            reports.push(None);
            continue;
        };

        let mut total_fee = I256::ZERO;
        let mut token_changes = BalanceChange::default();

        for (receipt, bcs) in &all_involved_txs {
            let mut fee = I256::ZERO;

            if wallet.involved_wallets().contains(&receipt.from) {
                fee = calculate_tx_fee(chain, receipt)?;
                total_fee += fee;
            }

            let recipient = receipt.from.eq(&wallet.address).then_some(receipt.to).flatten();
            let bc = merge_accounts(&bcs.filtered, wallet.involved_wallets(), recipient);

            trace!(
                tx.index = receipt.transaction_index.unwrap(),
                tx.hash = %receipt.transaction_hash,
                tx.fee = %fee,
                ?bcs.filtered,
                bc.merged = ?bc,
                wallet.involved_wallets = ?wallet.involved_wallets(),
            );

            token_changes.extend(&bc);
        }

        token_changes.retain_non_zero();

        let ether_pnl = token_changes.extract_ether(chain) - total_fee + I256::from_raw(builder_reward);

        let mut txs: Vec<TxAndPosition> = all_involved_txs
            .iter()
            .map(|(receipt, _)| TxAndPosition {
                index: receipt.transaction_index.unwrap(),
                hash: receipt.transaction_hash,
            })
            .collect();

        txs.sort_by_key(|t| t.index);

        reports.push(Some(PnlReport {
            txs,
            pnl: ether_pnl,
            token_changes,
            builder_reward,
            validator_bribe,
        }));
    }

    Ok(reports)
}

#[instrument(skip_all, fields(tx = %receipt.transaction_hash))]
pub fn generate_pnl(
    chain: Chain,
    receipt: &AnyTransactionReceipt,
    call_trace: &CallFrame,
    only_addresses: Option<&HashSet<Address>>,
) -> eyre::Result<BalanceChanges> {
    let mut bcs = BalanceChanges::default();
    if !receipt.status() {
        return Ok(bcs);
    }

    let mut stack = VecDeque::with_capacity(1024);
    stack.push_front(call_trace);

    let weth: Address = chain
        .named()
        .and_then(|c| c.wrapped_native_token())
        .context("WETH address not found. Chain is not supported")?
        .0
         .0
        .into();

    macro_rules! is_relevant_address {
        ($addr:expr) => {
            only_addresses.is_none() ||
                only_addresses
                    .as_ref()
                    .map(|set| set.contains($addr))
                    .unwrap_or_default()
        };
    }

    while let Some(frame) = stack.pop_front() {
        if frame.error.is_some() || frame.revert_reason.is_some() {
            // Skip reverted call
            continue;
        }

        for log in &frame.logs {
            let log = alloy::primitives::Log::new(
                log.address.context("Log address is not set")?,
                log.topics.clone().unwrap_or_default(),
                log.data.clone().unwrap_or_default(),
            )
            .context("Log is invalid")?;

            let (token, from, to, value) = if let Some(transfer) = primitive_log_decode::<ERC20::Transfer>(&log) {
                (log.address, transfer.from, transfer.to, transfer.value)
            } else if log.address.as_slice() == weth.as_slice() && is_weth9(chain) {
                if let Some(withdrawal) = primitive_log_decode::<WETH9::Withdrawal>(&log) {
                    (weth, withdrawal.src, Address::ZERO, withdrawal.wad)
                } else if let Some(deposit) = primitive_log_decode::<WETH9::Deposit>(&log) {
                    (weth, Address::ZERO, deposit.dst, deposit.wad)
                } else {
                    continue;
                }
            } else {
                continue;
            };

            if !is_relevant_address!(&from) && !is_relevant_address!(&to) {
                continue;
            }

            bcs.append_transfer(token, from, to, value);
        }

        stack.extend(frame.calls.iter());

        let value = frame.value.unwrap_or_default();
        if value.is_zero() {
            continue;
        }

        match frame.typ.as_str() {
            "CALL" | "CALLCODE" | "CREATE" | "CREATE2" | "SELFDESTRUCT" => {
                let to = frame.to.unwrap_or_default();
                let from = frame.from;

                if !is_relevant_address!(&from) && !is_relevant_address!(&to) {
                    continue;
                }

                bcs.append_transfer(NATIVE_TOKEN, from, to, value);
            }

            _ => continue,
        };
    }

    bcs.retain_non_zero();

    Ok(bcs)
}

pub fn trace_options() -> GethDebugTracingOptions {
    GethDebugTracingOptions::default()
        .with_tracer(GethDebugBuiltInTracerType::CallTracer.into())
        .with_call_config(CallConfig {
            only_top_call: Some(false),
            with_log: Some(true),
        })
}

fn find_all_receipients<'a>(
    receipts: impl Iterator<Item = &'a AnyTransactionReceipt>,
    wallets: &[WalletWithContext],
) -> HashSet<Address> {
    receipts
        // Only consider successful txs
        .filter(|r| r.status())
        // Only consider txs who has a recipient
        .filter_map(|r| r.to.map(|to| (r.from, to)))
        // Only consider wallets that include recipient and the transaction is from this wallet
        .filter_map(|(from, to)| {
            wallets
                .iter()
                .any(|w| w.include_recipient && w.address == from)
                .then_some(to)
        })
        .collect()
}

fn clone_and_retain_accounts(bcs: &BalanceChanges, accounts: &HashSet<Address>) -> BalanceChanges {
    let mut result = bcs.clone();

    for (account, bc) in bcs.iter() {
        if accounts.contains(account) {
            result.insert(*account, bc.clone());
        }
    }

    result
}

/// Check the balance changes generated from a tx matched the pattern of a
/// shitcoin airdrop.
/// Pattern: multiple tokens are transferred to multiple addresses.
fn is_shitcoin_airdrop(full_bcs: &BalanceChanges) -> bool {
    // There must be at least 3 accounts to have balance changes, 1 for sender, 2
    // for recipients
    if full_bcs.len() < 3 {
        return false;
    }

    let bc_sheet_iter = full_bcs
        .iter()
        .flat_map(|(acc, bc)| bc.iter().map(move |(t, amount)| (acc, t, amount)));

    let Some((_, token, _)) = bc_sheet_iter.clone().next() else {
        return false;
    };

    let same_token = bc_sheet_iter.clone().all(|(_, t, _)| t == token);
    if !same_token {
        return false;
    }

    let sender_count = bc_sheet_iter.filter(|(_, _, amount)| amount.is_negative()).count();
    if sender_count != 1 {
        return false;
    }

    true
}

fn merge_accounts(bcs: &BalanceChanges, accounts: &[Address], recipient: Option<Address>) -> BalanceChange {
    let mut bc = accounts
        .iter()
        .chain(recipient.iter())
        .collect::<HashSet<_>>() // Collect into HashSet to deduplicate
        .into_iter()
        .filter_map(|a| bcs.get(a))
        .fold(BalanceChange::default(), |mut acc, bc| {
            acc.extend(bc);
            acc
        });

    bc.retain_non_zero();

    bc
}

fn calculate_tx_fee(chain: Chain, receipt: &AnyTransactionReceipt) -> eyre::Result<I256> {
    let extra_cost = if chain.is_optimism() {
        let l1_fee = receipt
            .other
            .get("l1Fee")
            .and_then(|v| v.as_str())
            .map(|s| s.trim_start_matches("0x").to_string())
            .unwrap_or_default();

        U256::from_str_radix(&l1_fee, 16).map_err(|_| eyre!("Failed to parse l1Fee {l1_fee}"))?
    } else {
        U256::ZERO
    };

    let fee = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price) + extra_cost;
    Ok(I256::from_raw(fee))
}

fn calculate_builder_reward<'a>(
    base_fee: u128,
    receipts_iter: impl Iterator<Item = &'a AnyTransactionReceipt>,
) -> U256 {
    receipts_iter
        .map(|r| U256::from(r.effective_gas_price - base_fee) * U256::from(r.gas_used))
        .sum()
}

#[derive(Clone, Eq, PartialEq)]
pub struct TxAndPosition {
    pub index: u64,
    pub hash: TxHash,
}

impl std::fmt::Debug for TxAndPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:}:{}", self.hash, self.index)
    }
}

serde_with::serde_conv!(
    TxAndPositionAsStr,
    TxAndPosition,
    |tx: &TxAndPosition| format!("{:}:{}", tx.hash, tx.index),
    |s: String| -> eyre::Result<TxAndPosition> {
        let (hash, index) = s.split_once(':').context("Invalid format! Expecting <hash>:<index>")?;
        let tx: TxHash = hash.parse().context("Invalid tx hash")?;
        let index: u64 = index.parse().context("Invalid index")?;
        Ok(TxAndPosition { index, hash: tx })
    }
);

pub struct PnlReportTxFormatWrapper<'a>(&'a PnlReport);

impl<'a> std::fmt::Display for PnlReportTxFormatWrapper<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.txs.is_empty() {
            return Ok(());
        }

        if self.0.txs.len() == 1 {
            return write!(
                f,
                "{tx}:{position}",
                tx = self.0.txs[0].hash,
                position = self.0.txs[0].index
            );
        }

        for (i, tx_and_position) in self.0.txs.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }

            write!(f, "{}:{}", tx_and_position.hash, tx_and_position.index)?;
        }

        Ok(())
    }
}

struct BalanceChangesCache {
    filtered: BalanceChanges,
    full: BalanceChanges,
}

fn find_validator_bribe(all_bcs: &[BalanceChangesCache]) -> U256 {
    let Some(bc) = all_bcs.last() else {
        return U256::ZERO;
    };

    // Find the largest beneficary
    bc.full
        .iter()
        .filter_map(|(_, bc)| {
            let ether = match bc.get(&NATIVE_TOKEN) {
                Some(ether) if ether.is_positive() => ether.into_raw(),
                _ => return None,
            };

            Some(ether)
        })
        .max()
        .unwrap_or_default()
}
