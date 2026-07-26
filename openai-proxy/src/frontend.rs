mod envoy_xds;

use crate::{Endpoint, Error, config, endpoint};
use axum::{Router, extract, routing};
use futures::TryFutureExt;
use rand::distr::Distribution;
use std::sync::Arc;
use std::time::Duration;
use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    #[serde(flatten)]
    connection: Connection,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
enum Connection {
    #[serde(rename = "standard")]
    Standard {
        #[serde(flatten)]
        bind: config::Bind,
        body_limit: Option<usize>,
    },
    #[serde(rename = "tunnel")]
    Tunnel {
        #[serde(with = "http_serde::uri")]
        uri: http::Uri,
        authorization: Option<config::Authorization>,
        body_limit: Option<usize>,
    },
    #[serde(rename = "envoy-xds")]
    EnvoyXds {
        #[serde(flatten)]
        bind: config::Bind,
        route_config_name: String,
        metadata_namespace: String,
        template_cluster: Option<cluster_v3::Cluster>,
        template_route: Option<route_v3::Route>,
    },
}

type Receiver = tokio::sync::watch::Receiver<Option<(u64, Arc<[Endpoint]>)>>;

pub async fn serve(config: Vec<Config>, rx: Receiver) -> Result<(), Error> {
    futures::future::try_join_all(config.into_iter().map(|config| {
        let rx = rx.clone();
        async move {
            match config.connection {
                Connection::Standard { bind, body_limit } => {
                    serve_standard(bind, body_limit, rx).await
                }
                Connection::Tunnel {
                    uri,
                    authorization,
                    body_limit,
                } => serve_tunnel(uri, authorization, body_limit, rx).await,
                Connection::EnvoyXds {
                    bind,
                    route_config_name,
                    metadata_namespace,
                    template_cluster,
                    template_route,
                } => {
                    envoy_xds::serve(
                        bind,
                        route_config_name,
                        metadata_namespace,
                        template_cluster,
                        template_route,
                        rx,
                    )
                    .await
                }
            }
        }
    }))
    .map_ok(|_| ())
    .await
}

async fn serve_standard(
    bind: config::Bind,
    body_limit: Option<usize>,
    rx: Receiver,
) -> Result<(), Error> {
    let listener = bind.bind().await?;
    axum::serve(listener, axum_router(body_limit, rx)).await?;
    Ok(())
}

async fn serve_tunnel(
    uri: http::Uri,
    authorization: Option<config::Authorization>,
    body_limit: Option<usize>,
    rx: Receiver,
) -> Result<(), Error> {
    let service = hyper_util::service::TowerToHyperService::new(axum_router(body_limit, rx));
    let serve = async || -> Result<(), Error> {
        let mut builder = tokio_tungstenite::tungstenite::ClientRequestBuilder::new(uri.clone());
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

fn axum_router(body_limit: Option<usize>, rx: Receiver) -> Router {
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
                .flat_map(|endpoint| endpoint.models.clone())
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
    ) -> Result<http::Response<endpoint::Body>, http::StatusCode> {
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
                endpoint
                    .models
                    .iter()
                    .filter_map(move |model| (model.id == *model_id).then_some((endpoint, model)))
            })
            .collect::<Vec<_>>();

        if endpoints.is_empty() {
            Err(http::StatusCode::SERVICE_UNAVAILABLE)
        } else {
            let dist =
                rand::distr::weighted::WeightedIndex::new(endpoints.iter().map(|(_, model)| {
                    1. / (1. + model.metrics.vllm_num_requests_waiting.unwrap_or_default() as f64)
                }))
                .map_err(|e| {
                    tracing::warn!(error = e.to_string());
                    http::StatusCode::INTERNAL_SERVER_ERROR
                })?;

            let index = dist.sample(&mut rand::rng());
            let (endpoint, _) = &endpoints[index];
            let response = endpoint
                .endpoint
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

    Router::new()
        .route("/health", routing::get(health))
        .route("/v1/models", routing::get(list_models))
        .fallback(routing::any(fallback))
        .layer(tower::util::option_layer(
            body_limit.map(extract::DefaultBodyLimit::max),
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(rx)
}
