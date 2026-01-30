mod aggregated_discovery_service;
mod lb;
mod vllm;

use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::net::{Ipv4Addr, SocketAddr};

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[allow(clippy::large_enum_variant)]
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

fn default_bind() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 50051).into()
}

fn parse_json<T>(s: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(s).map_err(|e| e.to_string())
}
