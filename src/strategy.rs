use {
    crate::{
        config::WalletWithContext,
        message::MessageGenerator,
        processor::{self},
        utils::{self},
    },
    alloy::{providers::Provider, rpc::types::Block, transports::Transport},
    alloy_chains::Chain,
    burberry::{
        executor::telegram_message::{Message, MessageBuilder},
        ActionSubmitter, Strategy,
    },
    eyre::Context,
    std::sync::Arc,
    tokio::time::Instant,
    tracing::{error, info, instrument},
};

pub struct WalletWatcher<T: Clone + Transport> {
    pub provider: Arc<dyn Provider<T>>,
    pub chain: Chain,
    pub wallets: Vec<WalletWithContext>,
    pub message_generator: MessageGenerator<T>,
}

impl<T: Clone + Transport> WalletWatcher<T> {
    pub fn new(chain: Chain, provider: Arc<dyn Provider<T>>, wallets: Vec<WalletWithContext>) -> Self {
        Self {
            message_generator: MessageGenerator::new(chain, Arc::clone(&provider)),

            chain,
            provider,
            wallets,
        }
    }

    #[instrument(skip_all, fields(chain = %self.chain, block = block.header.number))]
    pub async fn process_block<A: From<Message> + Send + Sync + Clone + 'static>(
        &mut self,
        block: Block,
        submitter: Arc<dyn ActionSubmitter<A>>,
    ) -> eyre::Result<()> {
        let receipt_and_traces = utils::get_receipt_and_trace(self.provider.as_ref(), block.header.number)
            .await
            .context("Failed to get receipt and traces")?;

        let reports = processor::process_block(self.chain, &block.header, receipt_and_traces.as_slice(), &self.wallets)
            .context("Failed to generate balance changes")?;

        let report_and_wallet_index = reports
            .into_iter()
            .enumerate()
            .filter_map(|(i, r)| r.map(|r| (i, r)))
            .collect::<Vec<_>>();

        for (wallet_index, report) in report_and_wallet_index {
            info!(
                wallet = format_args!("{}-{:#x}", self.wallets[wallet_index].name, self.wallets[wallet_index].address),
                pnl = ?report.pnl,
                token_changes = ?report.token_changes,
                tx = %report.tx_formatter(),
            );

            let wallet = &self.wallets[wallet_index];

            let message = self
                .message_generator
                .generate(&block, &receipt_and_traces, &report, wallet)
                .await?;

            let mut mb = MessageBuilder::default()
                .bot_token(wallet.alert_to.bot_token.clone())
                .chat_id(wallet.alert_to.chat_id.clone())
                .text(message)
                .disable_link_preview(true);

            if let Some(thread_id) = &wallet.alert_to.thread_id {
                mb = mb.thread_id(thread_id.clone());
            }

            submitter.submit(mb.build().into());
        }

        Ok(())
    }
}

#[burberry::async_trait]
impl<T, E, A> Strategy<E, A> for WalletWatcher<T>
where
    T: Clone + Transport,
    E: TryInto<Block> + Send + Sync + Clone + 'static,
    A: From<Message> + Send + Sync + Clone + 'static,
{
    async fn process_event(&mut self, event: E, submitter: Arc<dyn ActionSubmitter<A>>) {
        let Ok(block) = event.try_into() else {
            return;
        };

        let block_num = block.header.number;

        let start = Instant::now();
        let result = self.process_block(block, submitter).await;
        let elapsed = start.elapsed();

        if let Err(err) = result {
            error!(
                chain = %self.chain,
                block = block_num,
                ?elapsed,
                "Failed to processed block: {err:#}");
        } else {
            info!(
                chain = %self.chain,
                block = block_num,
                ?elapsed,
                "Processed block");
        }
    }
}
