mod envoy_xds;
mod native_v1;

use super::{Receiver, connection};
use crate::Error;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config(Inner);

#[derive(Clone, Debug, serde::Deserialize)]
enum Inner {
    #[serde(rename = "native-v1")]
    NativeV1(native_v1::Config),
    #[serde(rename = "envoy-xds")]
    EnvoyXds(envoy_xds::Config),
}

pub(super) async fn serve(
    connection: connection::Config,
    config: Config,
    rx: Receiver,
) -> Result<(), Error> {
    match config.0 {
        Inner::NativeV1(config) => native_v1::serve(connection, config, rx).await,
        Inner::EnvoyXds(config) => envoy_xds::serve(connection, config, rx).await,
    }
}
