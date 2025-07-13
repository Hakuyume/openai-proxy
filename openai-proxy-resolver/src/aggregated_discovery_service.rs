use futures::future::Either;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use prost::Name;
use sha2::{Digest, Sha224};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::watch;
use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::endpoint::v3 as endpoint_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::extensions::upstreams::http::v3 as http_v3;
use tonic_envoy::envoy::service::discovery::v3 as discovery_v3;
use tonic_envoy::envoy::service::discovery::v3::aggregated_discovery_service_server::{
    AggregatedDiscoveryService, AggregatedDiscoveryServiceServer,
};
use tonic_envoy::envoy::r#type::matcher::v3 as matcher_v3;

pub(crate) fn service() -> (
    watch::Sender<Arc<Config>>,
    AggregatedDiscoveryServiceServer<Server>,
) {
    let (tx, rx) = watch::channel(Arc::default());
    (tx, AggregatedDiscoveryServiceServer::new(Server { rx }))
}

pub(crate) struct Server {
    rx: watch::Receiver<Arc<Config>>,
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct Config {
    clusters: Vec<cluster_v3::Cluster>,
    route_configurations: Vec<route_v3::RouteConfiguration>,
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
        let mut version_info = 0_u64;
        let mut config = Arc::<Config>::default();
        let stream = stream.map(move |item| match item {
            Either::Left(Ok(request)) => {
                tracing::info!(
                    request.version_info,
                    ?request.resource_names,
                    request.type_url,
                    request.response_nonce,
                );
                if request.response_nonce.is_empty() {
                    if request.type_url == cluster_v3::Cluster::type_url() {
                        Ok(vec![response(version_info, &config.clusters)?])
                    } else if request.type_url == cluster_v3::Cluster::type_url() {
                        Ok(vec![response(version_info, &config.route_configurations)?])
                    } else {
                        Ok(Vec::new())
                    }
                } else {
                    Ok(Vec::new())
                }
            }
            Either::Left(Err(e)) => Err(e),
            Either::Right(c) => {
                if config != c {
                    version_info += 1;
                    config = c;
                    Ok(vec![
                        response(version_info, &config.clusters)?,
                        response(version_info, &config.route_configurations)?,
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
    version_info: u64,
    resources: &[T],
) -> Result<discovery_v3::DiscoveryResponse, tonic::Status>
where
    T: prost::Name,
{
    Ok(discovery_v3::DiscoveryResponse {
        version_info: format!("v{version_info}"),
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

impl Config {
    pub(crate) fn new<I, V, K, N>(
        endpoints: I,
        header_name: K,
        route_configuration_name: N,
    ) -> Result<Self, prost::EncodeError>
    where
        I: IntoIterator<Item = (IpAddr, u16, V)>,
        V: Into<String>,
        K: Into<String>,
        N: Into<String>,
    {
        let mut clusters = HashMap::new();
        for (addr, port, header_value) in endpoints {
            let header_value = header_value.into();
            let cluster_name = format!("cluster_{}", hex::encode(Sha224::digest(&header_value)));
            clusters
                .entry(cluster_name.clone())
                .or_insert_with(|| (header_value, Vec::new()))
                .1
                .push((addr, port));
        }
        let header_name = header_name.into();

        let virtual_host = route_v3::VirtualHost {
            name: "local_service".to_owned(),
            domains: vec!["*".to_owned()],
            routes: clusters
                .iter()
                .map(|(cluster_name, (header_value, _))| {
                    route(&header_name, header_value, cluster_name)
                })
                .collect::<Result<_, _>>()?,
            ..route_v3::VirtualHost::default()
        };
        let route_configuration = route_v3::RouteConfiguration {
            name: route_configuration_name.into(),
            virtual_hosts: vec![virtual_host],
            ..route_v3::RouteConfiguration::default()
        };
        Ok(Self {
            clusters: clusters
                .iter()
                .map(|(cluster_name, (_, endpoints))| {
                    cluster(cluster_name, endpoints.iter().copied())
                })
                .collect::<Result<_, _>>()?,
            route_configurations: vec![route_configuration],
        })
    }
}

fn cluster<N, I>(name: N, endpoints: I) -> Result<cluster_v3::Cluster, prost::EncodeError>
where
    N: Into<String>,
    I: IntoIterator<Item = (IpAddr, u16)>,
{
    let name = name.into();
    let endpoints = endpoints.into_iter().map(|(addr, port)| {
        let address = core_v3::address::Address::SocketAddress(core_v3::SocketAddress {
            address: addr.to_string(),
            port_specifier: Some(core_v3::socket_address::PortSpecifier::PortValue(port as _)),
            ..core_v3::SocketAddress::default()
        });
        let lb_endpoint = endpoint_v3::LbEndpoint {
            host_identifier: Some(endpoint_v3::lb_endpoint::HostIdentifier::Endpoint(
                endpoint_v3::Endpoint {
                    address: Some(core_v3::Address {
                        address: Some(address),
                    }),
                    ..endpoint_v3::Endpoint::default()
                },
            )),
            ..endpoint_v3::LbEndpoint::default()
        };
        endpoint_v3::LocalityLbEndpoints {
            lb_endpoints: vec![lb_endpoint],
            ..endpoint_v3::LocalityLbEndpoints::default()
        }
    });
    let http2_protocol_options = {
        use http_v3::http_protocol_options::explicit_http_config::ProtocolConfig;
        let explicit_http_config = http_v3::http_protocol_options::ExplicitHttpConfig {
            protocol_config: Some(ProtocolConfig::Http2ProtocolOptions(
                core_v3::Http2ProtocolOptions::default(),
            )),
        };
        let http_protocol_options = http_v3::HttpProtocolOptions {
            upstream_protocol_options: Some(
                http_v3::http_protocol_options::UpstreamProtocolOptions::ExplicitHttpConfig(
                    explicit_http_config,
                ),
            ),
            ..http_v3::HttpProtocolOptions::default()
        };
        prost_types::Any::from_msg(&http_protocol_options)?
    };
    Ok(cluster_v3::Cluster {
        name: name.clone(),
        cluster_discovery_type: Some(cluster_v3::cluster::ClusterDiscoveryType::Type(
            cluster_v3::cluster::DiscoveryType::Static as _,
        )),
        load_assignment: Some(endpoint_v3::ClusterLoadAssignment {
            cluster_name: name.clone(),
            endpoints: endpoints.collect(),
            ..endpoint_v3::ClusterLoadAssignment::default()
        }),
        typed_extension_protocol_options: [(
            "envoy.extensions.upstreams.http.v3.HttpProtocolOptions".to_owned(),
            http2_protocol_options.clone(),
        )]
        .into_iter()
        .collect(),
        ..cluster_v3::Cluster::default()
    })
}

fn route<K, V, C>(
    header_name: K,
    header_value: V,
    cluster_name: C,
) -> Result<route_v3::Route, prost::EncodeError>
where
    K: Into<String>,
    V: Into<String>,
    C: Into<String>,
{
    Ok(route_v3::Route {
        r#match: Some(route_v3::RouteMatch {
            headers: vec![route_v3::HeaderMatcher {
                name: header_name.into(),
                header_match_specifier: Some(
                    route_v3::header_matcher::HeaderMatchSpecifier::StringMatch(
                        matcher_v3::StringMatcher {
                            match_pattern: Some(matcher_v3::string_matcher::MatchPattern::Exact(
                                header_value.into(),
                            )),
                            ..matcher_v3::StringMatcher::default()
                        },
                    ),
                ),
                ..route_v3::HeaderMatcher::default()
            }],
            path_specifier: Some(route_v3::route_match::PathSpecifier::Prefix("/".to_owned())),
            ..route_v3::RouteMatch::default()
        }),
        action: Some(route_v3::route::Action::Route(route_v3::RouteAction {
            cluster_specifier: Some(route_v3::route_action::ClusterSpecifier::Cluster(
                cluster_name.into(),
            )),
            ..route_v3::RouteAction::default()
        })),
        ..route_v3::Route::default()
    })
}
