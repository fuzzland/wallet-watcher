use {
    crate::{config::Config, strategy::WalletWatcher, utils::new_pubsub_provider},
    alloy::{providers::Provider, pubsub::PubSubFrontend, rpc::types::Block},
    alloy_chains::Chain,
    burberry::{
        collector::BlockCollector,
        executor::telegram_message::{Message, TelegramMessageDispatcher},
        Engine,
    },
    clap::Parser,
    std::sync::Arc,
    tokio::task::JoinHandle,
    tracing::{error, info},
};

#[derive(Debug, Clone, Parser)]
pub struct Args {
    /// The path to the config file
    #[arg(default_value = "config.toml", help = "The path to the config file")]
    config: String,
}

impl Args {
    pub async fn run(self) {
        tracing_subscriber::fmt::init();

        let config = Config::from_file(&self.config).expect("Failed to parse config");
        if config.chains.is_empty() {
            panic!("no chain is set up");
        }

        let wallets_by_chain = config.to_wallet_with_context_by_chain();

        let mut tasks: Vec<JoinHandle<_>> = vec![];
        for (name, rpc) in config.chains {
            let wallets = wallets_by_chain.get(&name).cloned().unwrap_or_default();
            let provider: Arc<dyn Provider<PubSubFrontend>> = new_pubsub_provider(&rpc)
                .await
                .expect("Failed to create provider")
                .into();

            let task = tokio::spawn(async move {
                let chain: Chain = match provider.get_chain_id().await {
                    Ok(c) => c.into(),
                    Err(err) => {
                        error!(%rpc, "fail to get chain id: {err:#}");
                        std::process::exit(-1);
                    }
                };

                let mut engine = Engine::<Block, Message>::new();

                engine.add_collector(Box::new(BlockCollector::new(provider.clone())));
                engine.add_strategy(Box::new(WalletWatcher::new(chain, provider.clone(), wallets)));
                engine.add_executor(Box::new(TelegramMessageDispatcher::new(None, None, None)));

                info!(%chain, %rpc, "Start monitoring");
                let _ = engine.run_and_join().await;

                error!(%chain, "Engine stopped");
            });

            tasks.push(task);
        }

        #[allow(clippy::never_loop)]
        for task in tasks {
            let _ = task.await;
            break;
        }
    }
}
