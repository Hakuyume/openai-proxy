use crate::{Error, client};
use futures::TryFutureExt;
use http_body_util::BodyExt;
use std::future;
use std::pin::Pin;
use std::time::Duration;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, tag = "version")]
pub(super) enum Config {
    #[serde(rename = "1")]
    V1 {
        #[serde(with = "humantime_serde")]
        retry_delay: Duration,
    },
}

pub(super) fn watch(
    client: client::Client,
    config: Config,
) -> impl futures::Stream<Item = Result<Vec<schemas::Provider>, Error>> + Send {
    struct State {
        client: client::Client,
        retry_delay: Duration,
        stream: Option<http_body_server_sent_events::Decode<client::Body>>,
        sleep: Option<tokio::time::Sleep>,
    }

    impl State {
        async fn next(&mut self) -> Result<Vec<schemas::Provider>, Error> {
            #[derive(Debug, thiserror::Error)]
            #[error("{0:?}")]
            struct StatusError(http::Response<bytes::Bytes>);

            #[derive(Debug, thiserror::Error)]
            #[error("closed")]
            struct ClosedError;

            let stream = if let Some(stream) = &mut self.stream {
                stream
            } else {
                let response = self
                    .client
                    .send(http::Request::get("/providers").body(http_body_util::Empty::new())?)
                    .await?;
                let (parts, body) = response.into_parts();
                if parts.status.is_success() {
                    Ok(self
                        .stream
                        .insert(http_body_server_sent_events::decode(body)))
                } else {
                    let body = body
                        .collect()
                        .map_ok(http_body_util::Collected::to_bytes)
                        .await?;
                    let response = http::Response::from_parts(parts, body);
                    Err(StatusError(response))
                }?
            };
            let mut stream = Pin::new(stream);
            loop {
                let future = future::poll_fn(|cx| stream.as_mut().poll_frame(cx));
                if let Ok(event) = future.await.ok_or(ClosedError)??.into_data()
                    && let Some(data) = event.data
                {
                    break Ok(serde_json::from_str(&data)?);
                }
            }
        }
    }

    let Config::V1 { retry_delay } = config;
    let state = State {
        client,
        retry_delay,
        stream: None,
        sleep: None,
    };
    futures::stream::unfold(state, async |mut state| {
        if let Some(sleep) = state.sleep.take() {
            sleep.await;
        }
        let item = match state
            .next()
            .inspect_err(|e| tracing::warn!(error = e.to_string()))
            .await
        {
            Ok(providers) => Some(Ok(providers)),
            Err(e) if client::is_closed(&e) => None,
            Err(e) => {
                state.stream = None;
                state.sleep = Some(tokio::time::sleep(state.retry_delay));
                Some(Err(e))
            }
        };
        Some((item?, state))
    })
}
