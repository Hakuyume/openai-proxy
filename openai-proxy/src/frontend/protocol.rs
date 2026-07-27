mod envoy_xds;
mod native;

use super::{Receiver, connection};
use crate::Error;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config(Inner);

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
enum Inner {
    #[serde(rename = "native")]
    Native(native::Config),
    #[serde(rename = "envoy-xds")]
    EnvoyXds(envoy_xds::Config),
}

pub(super) async fn serve(
    connection: connection::Config,
    config: Config,
    rx: Receiver,
) -> Result<(), Error> {
    match config.0 {
        Inner::Native(config) => native::serve(connection, config, rx).await,
        Inner::EnvoyXds(config) => envoy_xds::serve(connection, config, rx).await,
    }
}
