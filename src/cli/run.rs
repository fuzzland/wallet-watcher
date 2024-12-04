use {
    crate::{
        config::{AlertTo, WalletWithContext},
        message::MessageGenerator,
        processor::{self, trace_options},
        utils::{get_receipt_and_trace, new_provider},
    },
    alloy::{
        primitives::{Address, TxHash, U64},
        providers::Provider,
        rpc::{
            client::BatchRequest,
            types::{trace::geth::GethTrace, AnyTransactionReceipt},
        },
    },
    alloy_chains::Chain,
    clap::Parser,
    std::{collections::HashSet, sync::Arc},
};

#[derive(Debug, Clone, Parser)]
pub struct TxArgs {
    hash: TxHash,

    #[arg(short, long, env = "ETH_RPC_URL")]
    rpc_url: String,
}

impl TxArgs {
    pub async fn run(self) {
        tracing_subscriber::fmt::init();

        let provider = new_provider(&self.rpc_url).await.expect("Failed to create provider");

        let mut batch = BatchRequest::new(provider.client());

        let chain = batch.add_call::<_, U64>("eth_chainId", &()).unwrap();
        let receipt = batch
            .add_call::<_, AnyTransactionReceipt>("eth_getTransactionReceipt", &(self.hash,))
            .unwrap();
        let trace = batch
            .add_call::<_, GethTrace>("debug_traceTransaction", &(self.hash, trace_options()))
            .unwrap();

        batch.await.expect("Failed to send batch request");

        let chain: Chain = chain.await.expect("Failed to get chain id").to::<u64>().into();
        let receipt = receipt.await.expect("Failed to get transaction receipt");
        let trace = trace.await.expect("Failed to trace transaction");

        let call_trace = trace
            .try_into_call_frame()
            .expect("Failed to convert trace to call frame");

        let involved_wallets = HashSet::from([receipt.from, receipt.to.expect("No recipient")]);

        let bcs = processor::generate_pnl(chain, &receipt, &call_trace, Some(&involved_wallets))
            .expect("Failed to generate balance changes");

        println!("{:#?}", bcs);
    }
}

#[derive(Debug, Clone, Parser)]
pub struct BlockArgs {
    block: u64,

    #[arg(help = "The address of the wallet to monitor")]
    address: Address,

    #[arg(short, long, help = "The builder address of the wallet")]
    builder: Option<Address>,

    #[arg(short, long, env = "ETH_RPC_URL")]
    rpc_url: String,

    #[arg(
        short = 'a',
        long = "address",
        help = "Other addresses to include in PnL calculations",
        value_delimiter = ','
    )]
    other_addresses: Vec<Address>,

    #[arg(long, help = "If true, the recipient will be included in PnL calculations")]
    include_recipient: bool,
}

impl BlockArgs {
    pub async fn run(self) {
        tracing_subscriber::fmt::init();

        let provider = new_provider(&self.rpc_url).await.expect("Failed to create provider");
        let provider: Arc<dyn Provider<_>> = Arc::from(provider);
        let chain: Chain = provider.get_chain_id().await.expect("Failed to get chain id").into();
        let block = provider
            .get_block_by_number(self.block.into(), false)
            .await
            .expect("Failed to get block")
            .expect("Block not found");
        let receipt_and_traces = get_receipt_and_trace(provider.as_ref(), self.block)
            .await
            .expect("Failed to get receipt and trace");

        let wallets = vec![WalletWithContext::new(
            "Unnamed".to_string(),
            self.address,
            self.builder,
            self.other_addresses,
            self.include_recipient,
            Arc::new(AlertTo {
                bot_token: "".to_string(),
                chat_id: "".to_string(),
                thread_id: None,
            }),
        )];

        let report = processor::process_block(chain, &block.header, &receipt_and_traces, &wallets)
            .expect("Failed to generate balance changes")
            .first()
            .unwrap()
            .clone();

        println!("Report: {report:#?}");

        if let Some(report) = report {
            let mut message_generator = MessageGenerator::new(chain, Arc::clone(&provider));

            let message = message_generator
                .generate(&block, &receipt_and_traces, &report, &wallets[0])
                .await
                .expect("Failed to generate message");

            println!("Message:");
            println!("{message}");
        }
    }
}
