mod backend;

use axum::response::IntoResponse;
use axum::{extract, routing, Json, Router};
use bytes::Bytes;
use clap::{ArgAction, Parser};
use futures::TryFutureExt;
use http::StatusCode;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::fmt;
use std::future;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tower_http::trace::TraceLayer;
use tracing::Instrument;
use url::Url;

#[derive(Parser)]
pub(super) struct Args {
    #[clap(long)]
    bind: SocketAddr,
    #[clap(long, action = ArgAction::Append)]
    backend: Vec<Url>,
    #[clap(long, value_parser = humantime::parse_duration)]
    interval: Duration,
}

pub(super) async fn main(args: Args) -> anyhow::Result<()> {
    let state = Arc::new(State {
        rng: Mutex::new(StdRng::from_entropy()),
        backends: RwLock::default(),
    });

    tokio::spawn(poll_backends(state.clone(), args.backend, args.interval)?);

    let app = Router::new()
        .route("/v1/models", routing::get(v1_models))
        .route("/v1/chat/completions", routing::post(tunnel))
        .route("/v1/completions", routing::post(tunnel))
        .route("/v1/embeddings", routing::post(tunnel))
        .with_state(state.clone())
        .layer(TraceLayer::new_for_http())
        .route("/health", routing::get(|| future::ready(())));

    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown({
            let mut sigterm = signal(SignalKind::terminate())?;
            async move {
                sigterm.recv().await;
            }
        })
        .await?;

    Ok(())
}

struct State {
    rng: Mutex<StdRng>,
    backends: RwLock<Arc<[backend::Backend]>>,
}

fn poll_backends(
    state: Arc<State>,
    uris: Vec<Url>,
    interval: Duration,
) -> anyhow::Result<impl Future<Output = Infallible>> {
    let (config, mut opts) = hickory_resolver::system_conf::read_system_conf()?;
    opts.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
    opts.cache_size = 0;
    opts.try_tcp_on_error = true;
    let resolver = hickory_resolver::TokioAsyncResolver::tokio(config, opts);
    let mut interval = tokio::time::interval(interval);
    Ok(async move {
        loop {
            interval.tick().await;
            let backends = futures::future::join_all(uris.iter().map(|uri| {
                let resolver = &resolver;
                async move {
                    let lookup_ip = async {
                        if let Some(host) = uri.host_str() {
                            match resolver.lookup_ip(host).await {
                                Ok(lookup_ip) => Some(lookup_ip),
                                Err(e) => {
                                    tracing::warn!(error = e.to_string());
                                    None
                                }
                            }
                        } else {
                            tracing::warn!("missing host");
                            None
                        }
                        .into_iter()
                        .flatten()
                    }
                    .instrument(tracing::info_span!("resolve", ?uri))
                    .await;

                    futures::future::join_all(lookup_ip.map(|ip| async move {
                        let mut backend = backend::Backend::new(uri.clone(), ip).ok()?;
                        backend.fetch_models().await.ok()?;
                        Some(backend)
                    }))
                    .await
                }
            }))
            .await
            .into_iter()
            .flatten()
            .flatten()
            .collect();
            tracing::info!(?backends);
            *state.backends.write().unwrap() = backends;
        }
    })
}

async fn v1_models(extract::State(state): extract::State<Arc<State>>) -> Json<List<Model>> {
    let backends = state.backends.read().unwrap().clone();
    let models = backends
        .iter()
        .flat_map(backend::Backend::models)
        .cloned()
        .collect();
    Json(List { data: models })
}

async fn tunnel(
    extract::State(state): extract::State<Arc<State>>,
    parts: http::request::Parts,
    body: Bytes,
) -> Result<http::Response<hyper::body::Incoming>, axum::response::Response> {
    fn error<M>(code: StatusCode, message: M) -> axum::response::Response
    where
        M: fmt::Display,
    {
        #[derive(Serialize)]
        #[serde(tag = "object", rename = "error")]
        struct Error {
            #[serde(with = "http_serde::status_code")]
            code: StatusCode,
            message: String,
        }
        (
            code,
            Json(Error {
                code,
                message: message.to_string(),
            }),
        )
            .into_response()
    }

    let model = {
        #[derive(Deserialize)]
        struct Body {
            model: String,
        }

        serde_json::from_slice::<Body>(&body)
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?
            .model
    };
    tracing::info!(model);

    let backends = state.backends.read().unwrap().clone();
    let backends = backends
        .iter()
        .filter(|backend| backend.models().iter().any(|Model { id, .. }| *id == model))
        .collect::<Vec<_>>();
    let backend = backends
        .choose(&mut *state.rng.lock().unwrap())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "unknown model"))?;
    tracing::info!(?backend);

    backend
        .request(http::Request::from_parts(parts, body))
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))
        .await
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "list")]
struct List<T> {
    data: Vec<T>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "model")]
struct Model {
    id: String,
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
}
