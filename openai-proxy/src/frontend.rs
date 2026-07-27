mod connection;
mod protocol;

use crate::{Error, endpoint};
use futures::TryFutureExt;
use std::sync::Arc;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    connection: connection::Config,
    protocol: protocol::Config,
}

type Receiver = tokio::sync::watch::Receiver<Option<(usize, Arc<[endpoint::Endpoint]>)>>;

pub(super) async fn serve(config: Vec<Config>, rx: Receiver) -> Result<(), Error> {
    futures::future::try_join_all(
        config
            .into_iter()
            .map(|config| protocol::serve(config.connection, config.protocol, rx.clone())),
    )
    .map_ok(|_| ())
    .await
}
