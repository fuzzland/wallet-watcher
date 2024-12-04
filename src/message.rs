use {
    crate::{
        config::WalletWithContext,
        contract::ERC20::ERC20Instance,
        processor::PnlReport,
        utils::{self, format_ether_trimmed, format_short_address, format_token_amount},
    },
    alloy::{
        network::ReceiptResponse,
        primitives::{address, Address},
        providers::Provider,
        rpc::types::{trace::geth::CallFrame, AnyTransactionReceipt, Block},
        transports::Transport,
    },
    alloy_chains::Chain,
    burberry::executor::telegram_message::escape,
    eyre::{Context, ContextCompat},
    std::{
        collections::{hash_map::Entry, HashMap},
        fmt::Write,
        sync::Arc,
    },
    tracing::error,
};

pub struct MessageGenerator<T: Clone + Transport> {
    chain: Chain,
    provider: Arc<dyn Provider<T>>,
    token_info: HashMap<Address, (String, u8)>,
}

impl<T: Clone + Transport> MessageGenerator<T> {
    pub fn new(chain: Chain, provider: Arc<dyn Provider<T>>) -> Self {
        let mut token_info = HashMap::default();

        if chain == Chain::mainnet() {
            token_info.insert(
                address!("9f8F72aA9304c8B593d555F12eF6589cC3A579A2"),
                ("MKR".to_string(), 18),
            );
        }

        Self {
            chain,
            provider,
            token_info,
        }
    }

    async fn load_symbol_and_decimal(&mut self, token: &Address) -> eyre::Result<&(String, u8)> {
        let entry = self.token_info.entry(*token);

        match entry {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => {
                let erc20 = ERC20Instance::new(*token, self.provider.root());

                let symbol = erc20.symbol().call().await.context("Failed to get symbol for token")?;
                let decimal = erc20
                    .decimals()
                    .call()
                    .await
                    .context("Failed to get decimals for token")?;

                Ok(e.insert((symbol._0, decimal._0)))
            }
        }
    }

    pub async fn generate(
        &mut self,
        block: &Block,
        receipt_and_traces: &[(AnyTransactionReceipt, CallFrame)],
        report: &PnlReport,
        wallet: &WalletWithContext,
    ) -> eyre::Result<String> {
        let mut message_content = format!(
            "{address_link} · \\#{chain} · {block_link}{builder_tag}\n",
            address_link = utils::address_link(self.chain, &wallet.address, Some(escape(&wallet.name))),
            chain = escape(&self.chain.to_string().to_uppercase()),
            block_link = utils::block_link(self.chain, block.header.number),
            builder_tag = if report.builder_reward.is_zero() { "" } else { "\\[B\\]" },
        );

        let (sign, pnl) = report.pnl.into_sign_and_abs();

        let currency_symbol = self
            .chain
            .named()
            .and_then(|chain| chain.native_currency_symbol())
            .unwrap_or("ETH");

        writeln!(
            &mut message_content,
            "{symbol}: *{sign}{pnl}*",
            symbol = escape(currency_symbol),
            sign = escape(if sign.is_positive() { "" } else { "-" }),
            pnl = escape(&format_ether_trimmed(&pnl)),
        )?;

        if !report.token_changes.is_empty() {
            let chain = self.chain;
            for (token, change) in report.token_changes.iter() {
                let (symbol, decimals) = match self.load_symbol_and_decimal(token).await {
                    Ok((symbol, decimals)) => (TokenName::Symbol(symbol).to_string(), *decimals),
                    Err(err) => {
                        error!(%token, "Failed to load symbol for token: {err:#}");
                        (TokenName::Address(token).to_string(), 18)
                    }
                };

                writeln!(
                    &mut message_content,
                    "{token_link}: {amount}",
                    token_link =
                        utils::token_owner_link(chain, token, &wallet.address, Some(escape(&symbol.to_string())),),
                    amount = escape(&format_token_amount(change, decimals, 8)),
                )?;
            }
        }

        if !report.validator_bribe.is_zero() {
            writeln!(
                &mut message_content,
                r#"\VBribe: {pnl}"#,
                pnl = escape(&format_ether_trimmed(&report.validator_bribe)),
            )?;
        }

        let max_index_length = digit_count(report.txs.iter().map(|tx| tx.index).max().unwrap_or(0));

        for tx_and_position in &report.txs {
            let receipt = receipt_and_traces
                .get(tx_and_position.index as usize)
                .map(|(r, _)| r)
                .with_context(|| {
                    format!(
                        "Failed to find receipt and trace for tx at index {}",
                        tx_and_position.index
                    )
                })?;

            let index_indent = " ".repeat(max_index_length - digit_count(tx_and_position.index));

            writeln!(
                &mut message_content,
                r#"\[`{index_indent}{index}`\] {status}{tx_link} \[{phalcon_link}\]"#,
                index = tx_and_position.index,
                status = if receipt.inner.status() { "✓" } else { "✗" },
                tx_link = utils::tx_link(
                    self.chain,
                    &tx_and_position.hash,
                    Some(escape(&utils::format_short_hash(&tx_and_position.hash)))
                ),
                phalcon_link = utils::phalcon_tx(self.chain, &tx_and_position.hash, Some("Phalcon".to_string())),
            )?;
        }

        Ok(message_content)
    }
}

fn digit_count(n: u64) -> usize {
    n.to_string().len()
}

enum TokenName<'a> {
    Symbol(&'a str),
    Address(&'a Address),
}

impl std::fmt::Display for TokenName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenName::Symbol(symbol) => write!(f, "{:<.12}", symbol),
            TokenName::Address(address) => write!(f, "{}", format_short_address(address)),
        }
    }
}
