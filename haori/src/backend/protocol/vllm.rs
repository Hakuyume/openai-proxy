use crate::{Error, client};
use futures::TryFutureExt;
use http_body_util::BodyExt;
use std::time::Duration;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    #[serde(with = "humantime_serde")]
    interval: Duration,
    #[serde(with = "humantime_serde")]
    timeout: Duration,
}

pub(super) fn watch(
    client: client::Client,
    config: Config,
) -> impl futures::Stream<Item = Result<Vec<schemas::Provider>, Error>> + Send {
    struct State {
        client: client::Client,
        id: uuid::Uuid,
        interval: misc::time::Interval,
        timeout: Duration,
    }

    impl State {
        async fn next(&mut self) -> Result<schemas::Provider, Error> {
            self.interval.tick().await;
            let future =
                futures::future::try_join(list_models(&self.client), scrape_metrics(&self.client));
            let (models, metrics) = tokio::time::timeout(self.timeout, future).await??;
            let provider = schemas::Provider {
                id: self.id,
                models,
                metrics,
            };
            Ok(provider)
        }
    }

    let state = State {
        client,
        id: uuid::Uuid::new_v4(),
        interval: misc::time::interval(config.interval),
        timeout: config.timeout,
    };
    futures::stream::unfold(state, async |mut state| {
        let item = match state.next().await {
            Ok(provider) => Some(Ok(vec![provider])),
            Err(e) if client::is_closed(&e) => None,
            Err(e) => Some(Err(e)),
        };
        Some((item?, state))
    })
}

#[tracing::instrument(err(level = tracing::Level::WARN), skip_all)]
async fn list_models(client: &client::Client) -> Result<Vec<schemas::Model>, Error> {
    let response = get(client, "/v1/models").await?;
    let body = serde_json::from_slice::<schemas::List<_>>(response.body())?;
    Ok(body.data)
}

#[tracing::instrument(err(level = tracing::Level::WARN), skip_all)]
async fn scrape_metrics(client: &client::Client) -> Result<schemas::Metrics, Error> {
    let response = get(client, "/metrics").await?;
    Ok(misc::metrics::parse_vllm(str::from_utf8(response.body())?)?)
}

async fn get(client: &client::Client, uri: &str) -> Result<http::Response<bytes::Bytes>, Error> {
    #[derive(Debug, thiserror::Error)]
    #[error("{0:?}")]
    struct StatusError(http::Response<bytes::Bytes>);

    let response = client
        .send(http::Request::get(uri).body(http_body_util::Empty::new())?)
        .await?;
    let (parts, body) = response.into_parts();
    let body = body
        .collect()
        .map_ok(http_body_util::Collected::to_bytes)
        .await?;
    let response = http::Response::from_parts(parts, body);
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(StatusError(response).into())
    }
}
