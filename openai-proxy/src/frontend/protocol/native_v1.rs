use super::{Receiver, connection};
use crate::{Error, client};
use axum::{extract, routing};
use futures::TryFutureExt;
use rand::distr::Distribution;
use std::time::Duration;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Config {
    body_limit: Option<usize>,
}

pub(super) async fn serve(
    connection: connection::Config,
    config: Config,
    rx: Receiver,
) -> Result<(), Error> {
    let app = axum::Router::new()
        .route("/health", routing::get(health))
        .route("/v1/models", routing::get(list_models))
        .fallback(routing::any(fallback))
        .layer(tower::util::option_layer(
            config.body_limit.map(extract::DefaultBodyLimit::max),
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(rx);

    match connection {
        connection::Config::Standard { bind } => {
            let listener = bind.bind().await?;
            axum::serve(listener, app).await?;
        }
        connection::Config::Tunnel { uri, authorization } => {
            let service = hyper_util::service::TowerToHyperService::new(app);
            let serve = async || -> Result<(), Error> {
                let mut builder =
                    tokio_tungstenite::tungstenite::ClientRequestBuilder::new(uri.clone());
                if let Some(authorization) = &authorization {
                    builder = builder.with_header(
                        http::header::AUTHORIZATION.as_str(),
                        authorization.value().await?,
                    );
                }
                let (stream, _) = tokio_tungstenite::connect_async(builder).await?;
                hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .keep_alive_interval(Duration::from_secs(5))
                    .timer(hyper_util::rt::TokioTimer::new())
                    .serve_connection(misc::tungstenite::Io::new(stream), service.clone())
                    .await?;
                Ok(())
            };

            loop {
                if let Err(e) = serve().await {
                    tracing::warn!(error = e.to_string());
                    tokio::time::sleep(Duration::from_secs(1)).await
                }
            }
        }
    }

    Ok(())
}

async fn health(extract::State(rx): extract::State<Receiver>) -> http::StatusCode {
    if rx.borrow().is_some() {
        http::StatusCode::OK
    } else {
        http::StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn list_models(
    extract::State(rx): extract::State<Receiver>,
) -> Result<axum::Json<schemas::List<schemas::Model>>, http::StatusCode> {
    if let Some((_, endpoints)) = rx.borrow().clone() {
        let data = endpoints
            .iter()
            .flat_map(|endpoint| &endpoint.providers)
            .flat_map(|provider| &provider.models)
            .cloned()
            .collect();
        Ok(axum::Json(schemas::List { data }))
    } else {
        Err(http::StatusCode::SERVICE_UNAVAILABLE)
    }
}

async fn fallback(
    extract::State(rx): extract::State<Receiver>,
    parts: http::request::Parts,
    body: bytes::Bytes,
) -> Result<http::Response<client::Body>, http::StatusCode> {
    #[derive(serde::Deserialize)]
    struct Body {
        model: String,
    }

    let Body { model: model_id } = serde_json::from_slice(&body).map_err(|e| {
        tracing::warn!(warn = e.to_string());
        http::StatusCode::BAD_REQUEST
    })?;
    let model_id = &model_id;

    let (_, endpoints) = rx
        .borrow()
        .clone()
        .ok_or(http::StatusCode::SERVICE_UNAVAILABLE)?;
    let endpoints = endpoints
        .iter()
        .flat_map(|endpoint| {
            endpoint.providers.iter().filter_map(move |provider| {
                provider
                    .models
                    .iter()
                    .any(|model| model.id == *model_id)
                    .then_some((endpoint, provider))
            })
        })
        .collect::<Vec<_>>();

    if endpoints.is_empty() {
        Err(http::StatusCode::SERVICE_UNAVAILABLE)
    } else {
        let dist =
            rand::distr::weighted::WeightedIndex::new(endpoints.iter().map(|(_, provider)| {
                1. / (1.
                    + provider
                        .metrics
                        .vllm_num_requests_waiting
                        .unwrap_or_default() as f64)
            }))
            .map_err(|e| {
                tracing::warn!(error = e.to_string());
                http::StatusCode::INTERNAL_SERVER_ERROR
            })?;

        let index = dist.sample(&mut rand::rng());
        let (endpoint, _) = &endpoints[index];
        let response = endpoint
            .client
            .send(http::Request::from_parts(
                parts,
                http_body_util::Full::new(body),
            ))
            .map_err(|e| {
                tracing::warn!(error = e.to_string());
                http::StatusCode::BAD_GATEWAY
            })
            .await?;
        Ok(response)
    }
}
