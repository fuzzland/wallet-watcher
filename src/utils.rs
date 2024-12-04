use {
    crate::processor::trace_options,
    alloy::{
        hex,
        primitives::{Address, B256, I256, U256},
        providers::{IpcConnect, Provider, ProviderBuilder, WsConnect},
        pubsub::PubSubFrontend,
        rpc::{
            client::BatchRequest,
            types::{
                trace::geth::{CallFrame, TraceResult},
                AnyTransactionReceipt,
            },
        },
        transports::Transport,
    },
    alloy_chains::{Chain, NamedChain},
    eyre::{bail, ensure, Context},
};

pub async fn get_receipt_and_trace<T: Clone + Transport>(
    provider: &dyn Provider<T>,
    block: u64,
) -> eyre::Result<Vec<(AnyTransactionReceipt, CallFrame)>> {
    let mut batch = BatchRequest::new(provider.client());
    let block_num_hex = format!("{:#x}", block);

    let receipts = batch
        .add_call::<_, Vec<AnyTransactionReceipt>>("eth_getBlockReceipts", &(block_num_hex.clone(),))
        .unwrap();
    let traces = batch
        .add_call::<_, Vec<TraceResult>>("debug_traceBlockByNumber", &(block_num_hex, trace_options()))
        .unwrap();

    batch.await.context("Failed to send batch request")?;

    let receipts = receipts.await.context("Failed to get transaction receipt")?;
    let traces = traces.await.context("Failed to trace transaction")?;
    ensure!(
        receipts.len() == traces.len(),
        "Receipts and traces have different lengths"
    );

    let mut receipt_and_traces = Vec::with_capacity(receipts.len());

    for (receipt, trace_result) in receipts.into_iter().zip(traces.into_iter()) {
        let trace = match trace_result {
            TraceResult::Success { result, .. } => result,
            TraceResult::Error { error, tx_hash } => bail!("Failed to trace tx {tx_hash:?}: {error}"),
        };

        let trace = trace
            .try_into_call_frame()
            .with_context(|| format!("Trace result {:#x} is not a call frame", receipt.transaction_hash))?;

        receipt_and_traces.push((receipt, trace));
    }

    Ok(receipt_and_traces)
}

pub fn format_units(value: U256, decimals: u8, keep_decimal: u8) -> String {
    let formatted = alloy::primitives::utils::format_units(value, decimals).unwrap();

    formatted
        .find('.')
        .map(|dot| {
            let (integer, decimal) = formatted.split_at(dot);

            let decimal = decimal[1..].trim_end_matches('0');
            let decimal = decimal.trim_end_matches('.');

            if decimal.is_empty() {
                integer.to_string()
            } else {
                let keep_decimal = std::cmp::min(keep_decimal as usize, decimal.len());
                format!("{}.{}", integer, &decimal[..keep_decimal])
            }
        })
        .unwrap_or(formatted)
}

fn validate_log_topic<T: ::alloy::sol_types::SolEvent>(log: &::alloy::primitives::Log) -> bool {
    T::ANONYMOUS ||
        log.topics()
            .first()
            .map(|event_sig| event_sig.eq(&T::SIGNATURE_HASH))
            .unwrap_or_default()
}

pub fn primitive_log_decode<T: ::alloy::sol_types::SolEvent>(
    log: &::alloy::primitives::Log,
) -> Option<::alloy::primitives::Log<T>> {
    if !validate_log_topic::<T>(log) {
        return None;
    }

    T::decode_log(log, true).ok()
}

pub fn is_weth9(chain: Chain) -> bool {
    matches!(
        chain.named(),
        Some(NamedChain::Mainnet) |
            Some(NamedChain::BinanceSmartChain) |
            Some(NamedChain::Polygon) |
            Some(NamedChain::Optimism) |
            Some(NamedChain::Base) |
            Some(NamedChain::Blast)
    )
}

pub async fn new_provider(rpc: &str) -> eyre::Result<Box<dyn Provider>> {
    let p = if rpc.starts_with("http://") || rpc.starts_with("https://") {
        ProviderBuilder::new()
            .on_http(rpc.parse().context("Invalid rpc url")?)
            .boxed()
    } else {
        new_pubsub_provider(rpc).await?.root().clone().boxed()
    };

    Ok(Box::new(p))
}

pub async fn new_pubsub_provider(rpc: &str) -> eyre::Result<Box<dyn Provider<PubSubFrontend>>> {
    let p = if rpc.starts_with("ws://") || rpc.starts_with("wss://") {
        ProviderBuilder::new().on_ws(WsConnect::new(rpc)).await?
    } else if let Some(ipc_path) = rpc.strip_prefix("file://") {
        ProviderBuilder::new()
            .on_ipc(IpcConnect::new(ipc_path.to_owned()))
            .await?
    } else {
        bail!("Only WS and IPC are supported");
    };

    Ok(Box::new(p))
}

pub fn format_ether_trimmed(value: &U256) -> String {
    use std::fmt::Write;

    let mut result_string = String::with_capacity(64);
    let (ether_part, wei_part) = value.div_rem(U256::from(1_000_000_000_000_000_000_u128));

    write!(result_string, "{}", ether_part).unwrap();

    if !wei_part.is_zero() {
        write!(result_string, ".{:0>18}", wei_part.to_string()).unwrap();

        // trim 0 at the end in place
        result_string.truncate(result_string.trim_end_matches('0').len());
    }

    result_string
}

pub fn format_short_hash(hash: &B256) -> String {
    format!("0x{}..{}", hex::encode(&hash[..2]), hex::encode(&hash[30..]))
}

pub fn format_short_address(address: &Address) -> String {
    format!(
        "0x{}...{}",
        hex::encode(&address[..6]),
        hex::encode(&address[address.len() - 4..])
    )
}

fn prefix(chain: Chain) -> &'static str {
    chain
        .etherscan_urls()
        .map(|(_, url)| url)
        .unwrap_or("unsupported-chain")
}

pub fn tx_link(chain: Chain, hash: &B256, tag: Option<String>) -> String {
    format!(
        "[{tag_str}]({prefix}/tx/{hash})",
        tag_str = tag.unwrap_or_else(|| format!("{}", hash)),
        prefix = prefix(chain),
        hash = hash,
    )
}

pub fn address_link(chain: Chain, address: &Address, address_tag: Option<String>) -> String {
    format!(
        "[{tag}]({prefix}/address/{address})",
        tag = address_tag.unwrap_or_else(|| format!("{}", address)),
        address = address,
        prefix = prefix(chain),
    )
}

pub fn block_link(chain: Chain, block: u64) -> String {
    format!(
        "[{block}]({prefix}/block/{block})",
        block = block,
        prefix = prefix(chain)
    )
}

pub fn token_owner_link(chain: Chain, token: &Address, owner: &Address, owner_tag: Option<String>) -> String {
    format!(
        "[{tag}]({prefix}/token/{token}?a={owner})",
        tag = owner_tag.unwrap_or_else(|| format!("{owner}")),
        owner = owner,
        token = token,
        prefix = prefix(chain),
    )
}

pub fn phalcon_tx(chain: Chain, hash: &B256, tag: Option<String>) -> String {
    format!(
        "[{tag}](https://app.blocksec.com/explorer/tx/{chain_tag}/{hash})",
        tag = tag.unwrap_or_else(|| format!("{}", hash)),
        chain_tag = to_phalcon_chain_tag(chain),
    )
}

fn to_phalcon_chain_tag(chain: Chain) -> &'static str {
    match chain.named() {
        Some(NamedChain::Mainnet) => "eth",
        Some(NamedChain::Optimism) => "optimism",
        Some(NamedChain::Arbitrum) => "arbitrum",
        Some(NamedChain::BinanceSmartChain) => "bsc",
        Some(NamedChain::Gnosis) => "xdai",
        Some(NamedChain::Polygon) => "polygon",
        Some(NamedChain::Fantom) => "fantom",
        Some(NamedChain::Moonriver) => "moonriver",
        Some(NamedChain::Base) => "base",
        Some(NamedChain::Celo) => "celo",
        Some(NamedChain::Avalanche) => "avax",
        Some(NamedChain::Goerli) => "eth-goerli",
        Some(NamedChain::Sepolia) => "eth-sepolia",
        Some(NamedChain::Scroll) => "scroll",
        _ => "unsupported-chain",
    }
}

pub fn format_token_amount(value: &I256, decimals: u8, keep_decimal: u8) -> String {
    const MIN_AMOUNT: U256 = alloy::uint!(1_000_000_000_U256);
    let (sign, value) = value.into_sign_and_abs();
    let sign = if sign.is_positive() { "" } else { "-" };

    if decimals > 9 && value < MIN_AMOUNT {
        format!("{sign}{value} wei")
    } else {
        format!("{sign}{}", format_units(value, decimals, keep_decimal))
    }
}

serde_with::serde_conv!(
    pub U256AsDecimalStr,
    U256,
    ToString::to_string,
    |s: String| U256::from_str_radix(&s, 10)
);
