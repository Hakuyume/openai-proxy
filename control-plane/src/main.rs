mod aggregated_discovery_service;
mod lb;
mod vllm;

use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Lb(lb::Args),
    Vllm(vllm::Args),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    match args.command {
        Command::Lb(args) => lb::main(args).await,
        Command::Vllm(args) => vllm::main(args).await,
    }
}
