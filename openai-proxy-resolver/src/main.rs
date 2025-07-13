use clap::Parser;
use futures::stream::BoxStream;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::watch;
use tonic_envoy::envoy::service::discovery::v3::aggregated_discovery_service_server::{
    AggregatedDiscoveryService, AggregatedDiscoveryServiceServer,
};
use tonic_envoy::envoy::service::discovery::v3::{
    DeltaDiscoveryRequest, DeltaDiscoveryResponse, DiscoveryRequest, DiscoveryResponse,
};

#[derive(Parser)]
struct Args {
    #[clap(long, default_value_t = 50051)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(tonic_envoy::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    let (tx, rx) = watch::channel(Arc::default());
    let resolver = Resolver { rx };

    tonic::transport::Server::builder()
        .layer(tower_http::trace::TraceLayer::new_for_grpc())
        .add_service(reflection)
        .add_service(AggregatedDiscoveryServiceServer::new(resolver.clone()))
        .serve((Ipv4Addr::UNSPECIFIED, args.port).into())
        .await?;

    Ok(())
}

#[derive(Clone)]
struct Resolver {
    rx: watch::Receiver<Arc<[Upstream]>>,
}

#[derive(Debug)]
struct Upstream {
    uri: http::Uri,
    ip: IpAddr,
}

#[tonic::async_trait]
impl AggregatedDiscoveryService for Resolver {
    type StreamAggregatedResourcesStream =
        BoxStream<'static, Result<DiscoveryResponse, tonic::Status>>;
    async fn stream_aggregated_resources(
        &self,
        request: tonic::Request<tonic::Streaming<DiscoveryRequest>>,
    ) -> Result<tonic::Response<Self::StreamAggregatedResourcesStream>, tonic::Status> {
        let mut rx = self.rx.clone();
        todo!()
    }

    type DeltaAggregatedResourcesStream =
        BoxStream<'static, Result<DeltaDiscoveryResponse, tonic::Status>>;
    async fn delta_aggregated_resources(
        &self,
        request: tonic::Request<tonic::Streaming<DeltaDiscoveryRequest>>,
    ) -> Result<tonic::Response<Self::DeltaAggregatedResourcesStream>, tonic::Status> {
        let mut rx = self.rx.clone();
        todo!()
    }
}
