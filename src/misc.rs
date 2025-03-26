pub mod metrics;
pub mod pool;
pub mod schemas;

use axum::response::IntoResponse;
use futures::TryFutureExt;
use http::{Request, Response, StatusCode};
use http_body::Body;
use std::fmt;
use tower::{Service, ServiceBuilder, ServiceExt};

pub fn map_err<M>(code: StatusCode) -> impl FnOnce(M) -> axum::response::Response
where
    M: fmt::Display,
{
    move |message: M| {
        (
            code,
            axum::Json(schemas::Error {
                code,
                message: message.to_string(),
            }),
        )
            .into_response()
    }
}

pub fn v1_models<S, T, U>(
    service: S,
) -> impl Future<Output = anyhow::Result<Response<schemas::List<schemas::Model>>>>
where
    S: Clone + Service<Request<T>, Response = Response<U>>,
    S::Error: std::error::Error + Send + Sync + 'static,
    T: Default + Body,
    U: Default + Body,
    U::Error: std::error::Error + Send + Sync + 'static,
{
    let service = ServiceBuilder::new()
        .layer(http_extra::from_json::response::Layer::default())
        .layer(http_extra::check_status::Layer::default())
        .layer(http_extra::collect_body::response::Layer::default())
        .layer(tower_http::follow_redirect::FollowRedirectLayer::new())
        .service(service);
    async move {
        let request = Request::get("/v1/models").body(T::default())?;
        Ok(service.oneshot(request).await?)
    }
    .inspect_err(|e: &anyhow::Error| tracing::warn!(error = e.to_string()))
}
