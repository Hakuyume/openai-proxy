mod backend;
mod client;

use axum::response::IntoResponse;
use axum::{Json, Router, extract, routing};
use bytes::Bytes;
use clap::{ArgAction, Parser};
use futures::TryFutureExt;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use serde::Deserialize;
use std::fmt;
use std::future;
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
pub(super) struct Args {
    #[clap(long)]
    bind: SocketAddr,
    #[clap(long, action = ArgAction::Append)]
    backend: Vec<backend::Config>,
}

pub(super) async fn main(args: Args) -> anyhow::Result<()> {
    let prometheus_handle = crate::misc::metrics::install()?;

    let resolver = {
        let (config, mut opts) = hickory_resolver::system_conf::read_system_conf()?;
        opts.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
        opts.cache_size = 0;
        opts.try_tcp_on_error = true;
        hickory_resolver::TokioAsyncResolver::tokio(config, opts)
    };
    let pool = client::Pool::new()?;

    let (watch_backends, backends) = args
        .backend
        .into_iter()
        .map(|config| backend::watch(resolver.clone(), pool.clone(), config))
        .unzip::<_, _, Vec<_>, _>();
    tokio::spawn(futures::future::join_all(watch_backends));

    let state = Arc::new(State {
        rng: Mutex::new(StdRng::from_entropy()),
        backends,
    });

    let app = Router::new()
        .route("/v1/models", routing::get(v1_models))
        .route("/v1/chat/completions", routing::post(tunnel))
        .route("/v1/completions", routing::post(tunnel))
        .route("/v1/embeddings", routing::post(tunnel))
        .with_state(state.clone())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .route("/health", routing::get(|| future::ready(())))
        .route(
            "/metrics",
            routing::get(move || future::ready(prometheus_handle.render())),
        );

    crate::misc::metrics::serve(app, args.bind).await?;
    Ok(())
}

struct State {
    rng: Mutex<StdRng>,
    backends: Vec<backend::Backends>,
}

impl State {
    fn backends(&self) -> Vec<Arc<[backend::Backend]>> {
        self.backends
            .iter()
            .filter_map(|backends| Some(backends.read().ok()?.clone()))
            .collect()
    }
}

async fn v1_models(
    extract::State(state): extract::State<Arc<State>>,
) -> Json<crate::misc::schemas::List<crate::misc::schemas::Model>> {
    let backends = state.backends();
    let models = backends
        .iter()
        .flat_map(Deref::deref)
        .flat_map(backend::Backend::models)
        .cloned()
        .collect();
    Json(crate::misc::schemas::List { data: models })
}

async fn tunnel(
    extract::State(state): extract::State<Arc<State>>,
    parts: http::request::Parts,
    body: Bytes,
) -> Result<Response<hyper::body::Incoming>, axum::response::Response> {
    fn error<M>(code: StatusCode, message: M) -> axum::response::Response
    where
        M: fmt::Display,
    {
        (
            code,
            Json(crate::misc::schemas::Error {
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

    let backends = state.backends();
    let backends = backends
        .iter()
        .flat_map(Deref::deref)
        .filter(|backend| {
            backend
                .models()
                .iter()
                .any(|crate::misc::schemas::Model { id, .. }| *id == model)
        })
        .collect::<Vec<_>>();
    let backend = backends
        .choose(&mut *state.rng.lock().unwrap())
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "model not found"))?;

    tracing::info!(model, backends = backends.len(), ?backend);

    backend
        .send(Request::from_parts(parts, Full::new(body)))
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e))
        .await
}
