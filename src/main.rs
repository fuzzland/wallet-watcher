use clap::Parser;

mod balance_changes;
mod cli;
mod config;
mod contract;
mod message;
mod processor;
mod strategy;
mod utils;

#[tokio::main]
async fn main() {
    cli::Cli::parse().run().await.unwrap()
}
