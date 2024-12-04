use clap::{Parser, Subcommand};

mod backtest;
mod run;
mod start;

#[derive(Debug, Parser)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Start(start::Args),
    RunTx(run::TxArgs),
    RunBlock(run::BlockArgs),
    Backtest(backtest::Args),
}

impl Cli {
    pub async fn run(self) -> eyre::Result<()> {
        match self.command {
            Command::Start(args) => args.run().await,
            Command::RunTx(args) => args.run().await,
            Command::RunBlock(args) => args.run().await,
            Command::Backtest(args) => args.run().await,
        };

        Ok(())
    }
}
