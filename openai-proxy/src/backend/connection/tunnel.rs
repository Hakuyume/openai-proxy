use crate::{Error, client, config};
use futures::{StreamExt, TryFutureExt};
use std::future;
use std::time::Duration;
use tracing::Instrument;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config {
    bind: config::Bind,
    keep_alive_interval: Duration,
}

pub(super) async fn watch(
    config: Config,
) -> Result<
    impl futures::Stream<Item = Vec<(client::Client, futures::future::AbortRegistration)>> + Send,
    Error,
> {
    struct State {
        listener: tokio_net_incoming::Listener,
        keep_alive_interval: Duration,
    }

    impl State {
        async fn accept(
            &self,
        ) -> Result<(client::Client, futures::future::AbortRegistration), Error> {
            let (stream, _) = self.listener.accept().await?;
            let stream = tokio_tungstenite::accept_async(stream).await?;
            let (client, connection) = client::Client::tunnel(
                stream,
                client::tunnel::Config {
                    keep_alive_interval: self.keep_alive_interval,
                },
            )
            .await?;
            let (abort_guard, abort_registration) = misc::future::AbortGuard::new_pair();
            tokio::spawn(
                async move {
                    let _abort_guard = abort_guard;
                    connection.await
                }
                .inspect_err(|e| tracing::warn!(error = e.to_string()))
                .instrument(tracing::Span::current()),
            );
            Ok((client, abort_registration))
        }
    }

    let listener = config.bind.bind().await?;
    let state = State {
        listener,
        keep_alive_interval: config.keep_alive_interval,
    };
    let stream = futures::stream::unfold(state, async |state| {
        loop {
            match state.accept().await {
                Ok(item) => {
                    break Some((item, state));
                }
                Err(e) => {
                    tracing::warn!(error = e.to_string());
                }
            }
        }
    });
    Ok(futures::stream::once(future::ready(Vec::new())).chain(stream.map(|item| vec![item])))
}
