mod connection;
mod protocol;

use crate::{Error, endpoint};
use futures::{StreamExt, TryStreamExt};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    connection: connection::Config,
    protocol: protocol::Config,
}

pub(super) async fn watch(
    config: Vec<Config>,
) -> Result<impl futures::Stream<Item = Vec<endpoint::Endpoint>>, Error> {
    let mut builder = hickory_resolver::TokioResolver::builder_tokio()?;
    builder.options_mut().ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
    builder.options_mut().cache_size = 0;
    builder.options_mut().try_tcp_on_error = true;
    let resolver = builder.build()?;

    let mut streams = Vec::new();
    for config in config {
        let stream = connection::watch(resolver.clone(), config.connection).await?;
        let protocol = config.protocol;
        streams.push(
            stream
                .map(move |item| {
                    let protocol = protocol.clone();
                    item.into_iter().map(move |(client, abort_registration)| {
                        let stream = protocol::watch(client.clone(), protocol.clone());
                        let id = uuid::Uuid::new_v4();
                        futures::future::Abortable::new(stream, abort_registration)
                            .map_ok(move |providers| (id, client.clone(), providers))
                            .boxed()
                    })
                })
                .boxed(),
        );
    }
    let stream = misc::watch::watch(streams, |(id, client, providers)| endpoint::Endpoint {
        id: *id,
        client: client.clone(),
        providers: providers.clone(),
    });
    Ok(stream)
}
