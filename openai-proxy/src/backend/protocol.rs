mod vllm;

use crate::{Error, client};

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config(Inner);

#[derive(Clone, Debug, serde::Deserialize)]
enum Inner {
    #[serde(rename = "vllm")]
    Vllm(vllm::Config),
}

pub(super) fn watch(
    client: client::Client,
    config: Config,
) -> impl futures::Stream<Item = Result<Vec<schemas::Provider>, Error>> + Send {
    match config.0 {
        Inner::Vllm(config) => vllm::watch(client, config),
    }
}
