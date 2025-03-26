// mod backend;
// mod client;

use axum::{Json, Router, extract, routing};
use bytes::Bytes;
use futures::{FutureExt, TryFutureExt};
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Config {
    inner: Vec<crate::Config>,
}

pub(super) fn service(
    pool: &crate::misc::pool::Pool<Full<Bytes>>,
    config: Config,
) -> anyhow::Result<Router> {
    let rng = Arc::new(Mutex::new(StdRng::from_os_rng()));
    let services = config
        .inner
        .into_iter()
        .map(|config| crate::service(pool, config))
        .collect::<Result<_, _>>()?;
    let service = Router::new()
        .route("/v1/models", routing::get(v1_models))
        .route("/v1/chat/completions", routing::post(tunnel))
        .route("/v1/completions", routing::post(tunnel))
        .route("/v1/embeddings", routing::post(tunnel))
        .with_state((rng, services));
    Ok(service)
}

type State = (Arc<Mutex<StdRng>>, Arc<[Router]>);

async fn v1_models(
    extract::State((_, services)): extract::State<State>,
) -> Json<crate::misc::schemas::List<crate::misc::schemas::Model>> {
    let responses = futures::future::join_all(
        services
            .iter()
            .map(|service| crate::misc::v1_models::<_, Full<Bytes>, _>(service.clone())),
    )
    .await;
    let models = responses
        .into_iter()
        .flat_map(|response| {
            response.map_or_else(|_| Vec::new(), |response| response.into_body().data)
        })
        .collect();
    Json(crate::misc::schemas::List { data: models })
}

async fn tunnel(
    extract::State((rng, services)): extract::State<State>,
    parts: http::request::Parts,
    body: Bytes,
) -> Result<Response<axum::body::Body>, axum::response::Response> {
    let model = {
        #[derive(Deserialize)]
        struct Body {
            model: String,
        }

        serde_json::from_slice::<Body>(&body)
            .map_err(crate::misc::map_err(StatusCode::BAD_REQUEST))?
            .model
    };

    let responses = futures::future::join_all(services.iter().map(|service| {
        crate::misc::v1_models::<_, Full<Bytes>, _>(service.clone())
            .map(Result::ok)
            .map(move |response| (service, response))
    }))
    .await;

    let services = responses
        .into_iter()
        .filter_map(|(service, response)| {
            response
                .is_some_and(|response| {
                    response
                        .into_body()
                        .data
                        .iter()
                        .any(|crate::misc::schemas::Model { id, .. }| *id == model)
                })
                .then_some(service)
        })
        .collect::<Vec<_>>();
    let service = services
        .choose(&mut *rng.lock().unwrap())
        .ok_or_else(|| crate::misc::map_err(StatusCode::NOT_FOUND)("model not found"))?;

    (*service)
        .clone()
        .oneshot(Request::from_parts(parts, Full::new(body)))
        .map_err(crate::misc::map_err(StatusCode::BAD_GATEWAY))
        .await
}

// pub(super) async fn main(args: Args) -> anyhow::Result<()> {
//     let prometheus_handle = crate::misc::metrics::install()?;

//     let resolver = {
//         let mut builder = hickory_resolver::Resolver::builder_tokio()?;
//         builder.options_mut().ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4AndIpv6;
//         builder.options_mut().cache_size = 0;
//         builder.options_mut().try_tcp_on_error = true;
//         builder.build()
//     };
//     let pool = client::Pool::new()?;

//     let (watch_backends, backends) = args
//         .backend
//         .into_iter()
//         .map(|config| backend::watch(resolver.clone(), pool.clone(), config))
//         .unzip::<_, _, Vec<_>, _>();
//     tokio::spawn(futures::future::join_all(watch_backends));

//     let state = Arc::new(State {
//         rng: Mutex::new(StdRng::from_os_rng()),
//         backends,
//     });

//     let app = Router::new()
//         .route("/v1/models", routing::get(v1_models))
//         .route("/v1/chat/completions", routing::post(tunnel))
//         .route("/v1/completions", routing::post(tunnel))
//         .route("/v1/embeddings", routing::post(tunnel))
//         .with_state(state.clone())
//         .layer(tower_http::trace::TraceLayer::new_for_http())
//         .route("/health", routing::get(|| future::ready(())))
//         .route(
//             "/metrics",
//             routing::get(move || future::ready(prometheus_handle.render())),
//         );

//     crate::misc::metrics::serve(app, args.bind).await?;
//     Ok(())
// }

// struct State {
//     rng: Mutex<StdRng>,
//     backends: Vec<backend::Backends>,
// }

// impl State {
//     fn backends(&self) -> Vec<Arc<[backend::Backend]>> {
//         self.backends
//             .iter()
//             .filter_map(|backends| Some(backends.read().ok()?.clone()))
//             .collect()
//     }
// }

// async fn v1_models(
//     extract::State(state): extract::State<Arc<State>>,
// ) -> Json<crate::misc::schemas::List<crate::misc::schemas::Model>> {
//     let backends = state.backends();
//     let models = backends
//         .iter()
//         .flat_map(Deref::deref)
//         .flat_map(backend::Backend::models)
//         .cloned()
//         .collect();
//     Json(crate::misc::schemas::List { data: models })
// }

// async fn tunnel(
//     extract::State(state): extract::State<Arc<State>>,
//     parts: http::request::Parts,
//     body: Bytes,
// ) -> Result<Response<hyper::body::Incoming>, axum::response::Response> {
//     let model = {
//         #[derive(Deserialize)]
//         struct Body {
//             model: String,
//         }

//         serde_json::from_slice::<Body>(&body)
//             .map_err(crate::misc::map_err(StatusCode::BAD_REQUEST))?
//             .model
//     };

//     let backends = state.backends();
//     let backends = backends
//         .iter()
//         .flat_map(Deref::deref)
//         .filter(|backend| {
//             backend
//                 .models()
//                 .iter()
//                 .any(|crate::misc::schemas::Model { id, .. }| *id == model)
//         })
//         .collect::<Vec<_>>();
//     let backend = backends
//         .choose(&mut *state.rng.lock().unwrap())
//         .ok_or_else(|| crate::misc::map_err(StatusCode::NOT_FOUND)("model not found"))?;

//     tracing::info!(model, backends = backends.len(), ?backend);

//     backend
//         .send(Request::from_parts(parts, Full::new(body)))
//         .map_err(crate::misc::map_err(StatusCode::BAD_GATEWAY))
//         .await
// }
