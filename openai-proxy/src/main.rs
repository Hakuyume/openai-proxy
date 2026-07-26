mod backend;
mod config;
mod endpoint;
mod frontend;

use clap::Parser;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
struct Args {
    #[clap(long, value_parser = |s: &str| serde_json::from_str::<Config>(s))]
    config: Config,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Config {
    frontends: Vec<frontend::Config>,
    backends: Vec<backend::Config>,
}

#[derive(Debug)]
struct Endpoint {
    endpoint: endpoint::Endpoint,
    models: Vec<schemas::Model>,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let (tx, rx) = tokio::sync::watch::channel(None);
    futures::future::try_join(
        frontend::serve(args.config.frontends, rx),
        backend::watch(args.config.backends, tx),
    )
    .await?;
    Ok(())
}
