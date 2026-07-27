mod standard;
mod tunnel;

use crate::{Error, client};
use futures::StreamExt;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config(Inner);

#[derive(Clone, Debug, serde::Deserialize)]
enum Inner {
    #[serde(rename = "standard")]
    Standard(standard::Config),
    #[serde(rename = "tunnel")]
    Tunnel(tunnel::Config),
}

pub(super) async fn watch(
    resolver: hickory_resolver::TokioResolver,
    config: Config,
) -> Result<
    impl futures::Stream<Item = Vec<(client::Client, futures::future::AbortRegistration)>> + Send,
    Error,
> {
    let stream = match config.0 {
        Inner::Standard(config) => standard::watch(resolver.clone(), config)?.left_stream(),
        Inner::Tunnel(config) => tunnel::watch(config).await?.right_stream(),
    };
    Ok(stream)
}
