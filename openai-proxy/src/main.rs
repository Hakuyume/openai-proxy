mod backend;
mod client;
mod config;
mod endpoint;
mod frontend;
mod header;

use clap::Parser;
use futures::StreamExt;
use std::path::PathBuf;
use std::pin;

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
#[serde(deny_unknown_fields)]
struct Config {
    frontends: Vec<frontend::Config>,
    backends: Vec<backend::Config>,
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
    futures::future::try_join(frontend::serve(config.frontends, rx), async move {
        let stream = backend::watch(config.backends).await?.enumerate();
        let mut stream = pin::pin!(stream);
        while let Some((version, endpoints)) = stream.next().await {
            tx.send(Some((version, endpoints)))?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}
