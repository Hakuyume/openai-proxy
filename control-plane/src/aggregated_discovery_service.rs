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

pub(crate) type Service = AggregatedDiscoveryServiceServer<Server>;
pub(crate) fn service() -> (Reporter, Service) {
    let (clusters_tx, clusters_rx) = watch::channel(None);
    let (route_configurations_tx, route_configurations_rx) = watch::channel(None);
    (
        Reporter {
            clusters: clusters_tx,
            route_configurations: route_configurations_tx,
        },
        AggregatedDiscoveryServiceServer::new(Server {
            clusters: clusters_rx,
            route_configurations: route_configurations_rx,
        }),
    )
}

type Sender<T> = watch::Sender<Option<(Uuid, Arc<[T]>)>>;
type Receiver<T> = watch::Receiver<Option<(Uuid, Arc<[T]>)>>;

pub(crate) struct Reporter {
    clusters: Sender<cluster_v3::Cluster>,
    route_configurations: Sender<route_v3::RouteConfiguration>,
}

pub(crate) struct Server {
    clusters: Receiver<cluster_v3::Cluster>,
    route_configurations: Receiver<route_v3::RouteConfiguration>,
}

impl Reporter {
    pub(crate) fn clusters(
        &mut self,
        value: Vec<cluster_v3::Cluster>,
    ) -> Result<(), watch::error::SendError<()>> {
        Self::send(&self.clusters, value)
    }

    pub(crate) fn route_configurations(
        &mut self,
        value: Vec<route_v3::RouteConfiguration>,
    ) -> Result<(), watch::error::SendError<()>> {
        Self::send(&self.route_configurations, value)
    }

    fn send<T>(tx: &Sender<T>, value: Vec<T>) -> Result<(), watch::error::SendError<()>>
    where
        T: PartialEq,
    {
        let value = Arc::from(value);
        if tx
            .borrow()
            .as_ref()
            .is_none_or(|(_, current)| *current != value)
        {
            tx.send(Some((Uuid::new_v4(), value)))
                .map_err(|_| watch::error::SendError(()))?;
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
            futures::stream::select(
                tokio_stream::wrappers::WatchStream::new(self.clusters.clone()).map(Either::Left),
                tokio_stream::wrappers::WatchStream::new(self.route_configurations.clone())
                    .map(Either::Right),
            )
            .map(Either::Right),
        );
        let clusters = self.clusters.clone();
        let route_configurations = self.route_configurations.clone();
        let stream = stream.map(move |item| match item {
            Either::Left(request) => {
                let request = request?;
                tracing::info!(
                    request.version_info,
                    ?request.resource_names,
                    request.type_url,
                    request.response_nonce,
                );
                if request.type_url == cluster_v3::Cluster::type_url()
                    && request.response_nonce.is_empty()
                    && let Some((version_info, clusters)) = &*clusters.borrow()
                {
                    response(*version_info, clusters).map(Some)
                } else if request.type_url == route_v3::RouteConfiguration::type_url()
                    && request.response_nonce.is_empty()
                    && let Some((version_info, route_configurations)) =
                        &*route_configurations.borrow()
                {
                    response(*version_info, route_configurations).map(Some)
                } else {
                    Ok(None)
                }
            }
            Either::Right(Either::Left(Some((version_info, clusters)))) => {
                response(version_info, &clusters).map(Some)
            }
            Either::Right(Either::Right(Some((version_info, route_configurations)))) => {
                response(version_info, &route_configurations).map(Some)
            }
            _ => Ok(None),
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
