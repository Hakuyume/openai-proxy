mod native;
mod vllm;

use crate::{Error, client};
use futures::StreamExt;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config(Inner);

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
enum Inner {
    #[serde(rename = "native")]
    Native(native::Config),
    #[serde(rename = "vllm")]
    Vllm(vllm::Config),
}

pub(super) fn watch(
    client: client::Client,
    config: Config,
) -> impl futures::Stream<Item = Result<Vec<schemas::Provider>, Error>> + Send {
    match config.0 {
        Inner::Native(config) => native::watch(client, config).left_stream(),
        Inner::Vllm(config) => vllm::watch(client, config).right_stream(),
    }
}
