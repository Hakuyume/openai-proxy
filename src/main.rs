mod azure;
mod misc;
mod mux;

use clap::Parser;

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
enum Command {
    Azure(azure::Args),
    Mux(mux::Args),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    match args.command {
        Command::Azure(args) => azure::main(args).await,
        Command::Mux(args) => mux::main(args).await,
    }
}
