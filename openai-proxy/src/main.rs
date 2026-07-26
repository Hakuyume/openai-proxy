mod backend;
mod config;
mod endpoint;
mod frontend;

use clap::Parser;
use std::path::PathBuf;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
struct Args {
    #[clap(flatten)]
    config: ConfigArgs,
}

#[derive(clap::Args)]
#[group(multiple = false, required = true)]
struct ConfigArgs {
    #[clap(long)]
    config: Option<String>,
    #[clap(long)]
    config_path: Option<PathBuf>,
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
    let config: Config = match &args.config {
        ConfigArgs {
            config: Some(config),
            config_path: None,
        } => serde_json::from_str(config)?,
        ConfigArgs {
            config: None,
            config_path: Some(config_path),
        } => {
            let config = tokio::fs::read(config_path).await?;
            if config_path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                toml::from_slice(&config)?
            } else {
                serde_json::from_slice(&config)?
            }
        }
        _ => unreachable!(),
    };

    let (tx, rx) = tokio::sync::watch::channel(None);
    futures::future::try_join(
        frontend::serve(config.frontends, rx),
        backend::watch(config.backends, tx),
    )
    .await?;
    Ok(())
}
