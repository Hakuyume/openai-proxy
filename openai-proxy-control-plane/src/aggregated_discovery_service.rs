use futures::future::Either;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use prost::Name;
use std::sync::Arc;
use tokio::sync::watch;
use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::service::discovery::v3 as discovery_v3;
use tonic_envoy::envoy::service::discovery::v3::aggregated_discovery_service_server::{
    AggregatedDiscoveryService, AggregatedDiscoveryServiceServer,
};
use uuid::Uuid;

pub(crate) fn service() -> (Reporter, AggregatedDiscoveryServiceServer<Server>) {
    let (tx, rx) = watch::channel(None);
    (
        Reporter { tx, state: None },
        AggregatedDiscoveryServiceServer::new(Server { rx }),
    )
}

pub(crate) struct Reporter {
    tx: watch::Sender<Option<(Uuid, Arc<State>)>>,
    state: Option<Arc<State>>,
}

pub(crate) struct Server {
    rx: watch::Receiver<Option<(Uuid, Arc<State>)>>,
}

#[derive(PartialEq)]
pub(crate) struct State {
    pub(crate) clusters: Vec<cluster_v3::Cluster>,
    pub(crate) route_configurations: Vec<route_v3::RouteConfiguration>,
}

impl Reporter {
    pub(crate) fn update(&mut self, state: State) -> Result<(), watch::error::SendError<()>> {
        let state = Arc::new(state);
        if self.state.as_ref() != Some(&state) {
            self.tx
                .send(Some((Uuid::new_v4(), state.clone())))
                .map_err(|_| watch::error::SendError(()))?;
            self.state = Some(state);
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl AggregatedDiscoveryService for Server {
    type StreamAggregatedResourcesStream =
        BoxStream<'static, Result<discovery_v3::DiscoveryResponse, tonic::Status>>;
    async fn stream_aggregated_resources(
        &self,
        request: tonic::Request<tonic::Streaming<discovery_v3::DiscoveryRequest>>,
    ) -> Result<tonic::Response<Self::StreamAggregatedResourcesStream>, tonic::Status> {
        let stream = futures::stream::select(
            request.into_inner().map(Either::Left),
            tokio_stream::wrappers::WatchStream::new(self.rx.clone()).map(Either::Right),
        );
        let mut state = None::<(Uuid, Arc<State>)>;
        let stream = stream.map(move |item| match item {
            Either::Left(Ok(request)) => {
                tracing::info!(
                    request.version_info,
                    ?request.resource_names,
                    request.type_url,
                    request.response_nonce,
                );
                if request.response_nonce.is_empty()
                    && let Some((version_info, state)) = &state
                {
                    if request.type_url == cluster_v3::Cluster::type_url() {
                        Ok(vec![response(*version_info, &state.clusters)?])
                    } else if request.type_url == cluster_v3::Cluster::type_url() {
                        Ok(vec![response(*version_info, &state.route_configurations)?])
                    } else {
                        Ok(Vec::new())
                    }
                } else {
                    Ok(Vec::new())
                }
            }
            Either::Left(Err(e)) => Err(e),
            Either::Right(s) => {
                state = s;
                if let Some((version_info, state)) = &state {
                    Ok(vec![
                        response(*version_info, &state.clusters)?,
                        response(*version_info, &state.route_configurations)?,
                    ])
                } else {
                    Ok(Vec::new())
                }
            }
        });
        Ok(tonic::Response::new(
            stream
                .map_ok(|responses| futures::stream::iter(responses.into_iter().map(Ok)))
                .try_flatten()
                .inspect_ok(|response| {
                    tracing::info!(response.version_info, response.type_url, response.nonce)
                })
                .boxed(),
        ))
    }

    type DeltaAggregatedResourcesStream =
        futures::stream::Pending<Result<discovery_v3::DeltaDiscoveryResponse, tonic::Status>>;
    async fn delta_aggregated_resources(
        &self,
        _: tonic::Request<tonic::Streaming<discovery_v3::DeltaDiscoveryRequest>>,
    ) -> Result<tonic::Response<Self::DeltaAggregatedResourcesStream>, tonic::Status> {
        Err(tonic::Status::unimplemented(""))
    }
}

#[allow(clippy::result_large_err)]
fn response<T>(
    version_info: Uuid,
    resources: &[T],
) -> Result<discovery_v3::DiscoveryResponse, tonic::Status>
where
    T: prost::Name,
{
    Ok(discovery_v3::DiscoveryResponse {
        version_info: version_info.to_string(),
        resources: resources
            .iter()
            .map(prost_types::Any::from_msg)
            .collect::<Result<_, _>>()
            .map_err(|e| tonic::Status::internal(e.to_string()))?,
        type_url: T::type_url(),
        nonce: uuid::Uuid::new_v4().to_string(),
        ..discovery_v3::DiscoveryResponse::default()
    })
}
