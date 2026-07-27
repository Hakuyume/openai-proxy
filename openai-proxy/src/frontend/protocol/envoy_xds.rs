use super::{Receiver, connection};
use crate::{Error, client, endpoint};
use futures::future::Either;
use futures::{StreamExt, TryFutureExt, TryStreamExt};
use prost::Name;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::endpoint::v3 as endpoint_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::extensions::upstreams::http::v3 as http_v3;
use tonic_envoy::envoy::service::discovery::v3 as discovery_v3;
use tonic_envoy::envoy::service::discovery::v3::aggregated_discovery_service_server;
use tonic_envoy::envoy::r#type::matcher::v3 as matcher_v3;

pub(super) type Config = Arc<Inner>;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Inner {
    route_config_name: String,
    metadata_namespace: String,
    template_cluster: Option<cluster_v3::Cluster>,
    template_route: Option<route_v3::Route>,
}

pub(super) async fn serve(
    connection: connection::Config,
    config: Config,
    mut rx: Receiver,
) -> Result<(), Error> {
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET)
        .register_encoded_file_descriptor_set(tonic_envoy::FILE_DESCRIPTOR_SET)
        .build_v1()?;
    let (health_reporter, health_service) = tonic_health::server::health_reporter();

    let server = tonic::transport::Server::builder()
        .layer(tower_http::trace::TraceLayer::new_for_grpc())
        .add_service(reflection_service)
        .add_service(health_service)
        .add_service(
            aggregated_discovery_service_server::AggregatedDiscoveryServiceServer::new(Server {
                config,
                rx: rx.clone(),
            }),
        );

    match connection {
        connection::Config::Standard { bind } => {
            futures::future::try_join(
                async {
                    let listener = bind.bind().await?;
                    server
                        .serve_with_incoming(tokio_net_incoming::ListenerStream::new(listener))
                        .await?;
                    Ok(())
                },
                async {
                    type Service =
                        aggregated_discovery_service_server::AggregatedDiscoveryServiceServer<
                            Server,
                        >;
                    loop {
                        if rx.borrow_and_update().is_some() {
                            health_reporter.set_serving::<Service>().await;
                        } else {
                            health_reporter.set_not_serving::<Service>().await;
                        }
                        rx.changed().await?;
                    }
                },
            )
            .map_ok(|_: (_, Infallible)| ())
            .await
        }
        connection::Config::Tunnel { .. } => Err("`tunnel` and `envoy-xds` do not work together")?,
    }
}

struct Server {
    config: Config,
    rx: super::Receiver,
}

#[tonic::async_trait]
impl aggregated_discovery_service_server::AggregatedDiscoveryService for Server {
    type StreamAggregatedResourcesStream =
        futures::stream::BoxStream<'static, Result<discovery_v3::DiscoveryResponse, tonic::Status>>;
    async fn stream_aggregated_resources(
        &self,
        request: tonic::Request<tonic::Streaming<discovery_v3::DiscoveryRequest>>,
    ) -> Result<tonic::Response<Self::StreamAggregatedResourcesStream>, tonic::Status> {
        let stream = futures::stream::select(
            request.into_inner().map(Either::Left),
            tokio_stream::wrappers::WatchStream::new(self.rx.clone()).map(Either::Right),
        );
        let config = self.config.clone();
        let rx = self.rx.clone();
        let mut clusters = None;
        let mut route_configurations = None;
        let stream = stream.map(move |item| -> Result<_, tonic::Status> {
            let mut responses = Vec::new();
            match item {
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
                        && let Some((version, endpoints)) = rx.borrow().clone()
                    {
                        let (clusters_next, _) = generate(&config, &endpoints)
                            .map_err(|e| tonic::Status::internal(e.to_string()))?;
                        if clusters
                            .as_ref()
                            .is_none_or(|clusters| *clusters != clusters_next)
                        {
                            responses.push(response(version, &clusters_next)?);
                        }
                        clusters = Some(clusters_next);
                    } else if request.type_url == route_v3::RouteConfiguration::type_url()
                        && request.response_nonce.is_empty()
                        && let Some((version, endpoints)) = rx.borrow().clone()
                    {
                        let (_, route_configuration_next) = generate(&config, &endpoints)
                            .map_err(|e| tonic::Status::internal(e.to_string()))?;
                        let route_configurations_next = [route_configuration_next];
                        if route_configurations
                            .as_ref()
                            .is_none_or(|route_configurations| {
                                *route_configurations != route_configurations_next
                            })
                        {
                            responses.push(response(version, &route_configurations_next)?);
                        }
                        route_configurations = Some(route_configurations_next);
                    }
                }
                Either::Right(Some((version, endpoints))) => {
                    let (clusters_next, route_configuration_next) =
                        generate(&config, &endpoints)
                            .map_err(|e| tonic::Status::internal(e.to_string()))?;
                    if clusters
                        .as_ref()
                        .is_none_or(|clusters| *clusters != clusters_next)
                    {
                        responses.push(response(version, &clusters_next)?);
                    }
                    clusters = Some(clusters_next);
                    let route_configurations_next = [route_configuration_next];
                    if route_configurations
                        .as_ref()
                        .is_none_or(|route_configurations| {
                            *route_configurations != route_configurations_next
                        })
                    {
                        responses.push(response(version, &route_configurations_next)?);
                    }
                    route_configurations = Some(route_configurations_next);
                }
                _ => (),
            }
            Ok(responses)
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

fn generate(
    config: &Config,
    endpoints: &[endpoint::Endpoint],
) -> Result<(Vec<cluster_v3::Cluster>, route_v3::RouteConfiguration), Error> {
    fn cluster_name(endpoint_id: uuid::Uuid) -> String {
        format!("cluster_{}", endpoint_id.simple())
    }

    let endpoints = endpoints
        .iter()
        .filter_map(|endpoint| {
            if let client::Config::Standard(client::standard::Config {
                uri,
                http2_prior_knowledge,
                resolve: Some(resolve),
                unix_socket: None,
                authorization: None,
            }) = endpoint.client.config()
                && uri.scheme() == Some(&http::uri::Scheme::HTTP)
            {
                let port = if let Some(port) = uri.port_u16() {
                    port
                } else if resolve.port() > 0 {
                    resolve.port()
                } else {
                    80
                };
                Some((endpoint, (resolve.ip(), port, *http2_prior_knowledge)))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    let mut clusters = Vec::new();
    for (endpoint, (ip, port, http2_prior_knowledge)) in &endpoints {
        let address = core_v3::address::Address::SocketAddress(core_v3::SocketAddress {
            address: ip.to_string(),
            port_specifier: Some(core_v3::socket_address::PortSpecifier::PortValue(
                *port as _,
            )),
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

        let mut cluster = config.template_cluster.clone().unwrap_or_default();
        cluster.name = cluster_name(endpoint.id);
        cluster.cluster_discovery_type = Some(cluster_v3::cluster::ClusterDiscoveryType::Type(
            cluster_v3::cluster::DiscoveryType::Static as _,
        ));
        let load_assignment = cluster.load_assignment.get_or_insert_default();
        load_assignment.cluster_name = cluster_name(endpoint.id);
        load_assignment
            .endpoints
            .push(endpoint_v3::LocalityLbEndpoints {
                lb_endpoints: vec![lb_endpoint],
                ..endpoint_v3::LocalityLbEndpoints::default()
            });

        if *http2_prior_knowledge {
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
            cluster.typed_extension_protocol_options.insert(
                "envoy.extensions.upstreams.http.v3.HttpProtocolOptions".to_owned(),
                misc::pbjson::from_msg(&http_protocol_options)?,
            );
        }

        clusters.push(cluster);
    }

    let mut virtual_host = route_v3::VirtualHost {
        name: "local_service".to_owned(),
        domains: vec!["*".to_owned()],
        ..route_v3::VirtualHost::default()
    };

    {
        let mut data = endpoints
            .iter()
            .flat_map(|(endpoint, _)| &endpoint.providers)
            .flat_map(|provider| &provider.models)
            .collect::<Vec<_>>();
        data.sort_unstable_by_key(|model| &model.id);
        let body = serde_json::to_string(&schemas::List { data })?;

        let mut route = config.template_route.clone().unwrap_or_default();
        let match_ = route.r#match.get_or_insert_default();
        match_.path_specifier = Some(route_v3::route_match::PathSpecifier::Path(
            "/v1/models".to_owned(),
        ));
        match_.headers.push(route_v3::HeaderMatcher {
            name: ":method".to_owned(),
            header_match_specifier: Some(
                route_v3::header_matcher::HeaderMatchSpecifier::StringMatch(
                    matcher_v3::StringMatcher {
                        match_pattern: Some(matcher_v3::string_matcher::MatchPattern::Exact(
                            "GET".to_owned(),
                        )),
                        ..matcher_v3::StringMatcher::default()
                    },
                ),
            ),
            ..route_v3::HeaderMatcher::default()
        });
        route
            .response_headers_to_add
            .push(core_v3::HeaderValueOption {
                header: Some(core_v3::HeaderValue {
                    key: "content-type".to_owned(),
                    value: "application/json".to_owned(),
                    ..core_v3::HeaderValue::default()
                }),
                ..core_v3::HeaderValueOption::default()
            });
        let action = misc::get_or_insert_default!(
            &mut route.action,
            route_v3::route::Action::DirectResponse
        );
        action.status = http::StatusCode::OK.as_u16() as _;
        action.body.get_or_insert_default().specifier =
            Some(core_v3::data_source::Specifier::InlineString(body));
        virtual_host.routes.push(route);
    }

    {
        let mut models = BTreeMap::<_, BTreeMap<_, Vec<_>>>::new();
        for (endpoint, _) in &endpoints {
            for provider in &endpoint.providers {
                for model in &provider.models {
                    models
                        .entry(&model.id)
                        .or_default()
                        .entry(endpoint.id)
                        .or_default()
                        .push(
                            provider
                                .metrics
                                .vllm_num_requests_waiting
                                .unwrap_or_default(),
                        );
                }
            }
        }

        for (model_id, endpoints) in models {
            let waiting_max = endpoints
                .values()
                .flat_map(|endpoints| endpoints.iter().copied())
                .max()
                .unwrap_or_default();

            let mut route = config.template_route.clone().unwrap_or_default();
            let match_ = route.r#match.get_or_insert_default();
            match_.path_specifier =
                Some(route_v3::route_match::PathSpecifier::Prefix("/".to_owned()));
            match_.dynamic_metadata.push(matcher_v3::MetadataMatcher {
                filter: config.metadata_namespace.clone(),
                path: vec![matcher_v3::metadata_matcher::PathSegment {
                    segment: Some(matcher_v3::metadata_matcher::path_segment::Segment::Key(
                        "model".to_owned(),
                    )),
                }],
                value: Some(matcher_v3::ValueMatcher {
                    match_pattern: Some(matcher_v3::value_matcher::MatchPattern::StringMatch(
                        matcher_v3::StringMatcher {
                            match_pattern: Some(matcher_v3::string_matcher::MatchPattern::Exact(
                                model_id.clone(),
                            )),
                            ..matcher_v3::StringMatcher::default()
                        },
                    )),
                }),
                ..matcher_v3::MetadataMatcher::default()
            });
            let action =
                misc::get_or_insert_default!(&mut route.action, route_v3::route::Action::Route);
            let cluster_specifier = misc::get_or_insert_default!(
                &mut action.cluster_specifier,
                route_v3::route_action::ClusterSpecifier::WeightedClusters
            );
            cluster_specifier.clusters.extend(endpoints.into_iter().map(
                |(endpoint_id, waiting)| {
                    route_v3::weighted_cluster::ClusterWeight {
                        name: cluster_name(endpoint_id),
                        weight: Some(pbjson_types::UInt32Value::from(
                            waiting
                                .into_iter()
                                .map(|waiting| (1 + waiting_max) / (1 + waiting))
                                .sum::<u32>(),
                        )),
                        ..route_v3::weighted_cluster::ClusterWeight::default()
                    }
                },
            ));
            virtual_host.routes.push(route);
        }
    }

    let mut route_configuration = route_v3::RouteConfiguration {
        name: config.route_config_name.clone(),
        virtual_hosts: vec![virtual_host],
        ..route_v3::RouteConfiguration::default()
    };
    patch_max_direct_response_body_size_bytes(&mut route_configuration);

    Ok((clusters, route_configuration))
}

#[allow(clippy::result_large_err)]
fn response<T>(
    version_info: usize,
    resources: &[T],
) -> Result<discovery_v3::DiscoveryResponse, tonic::Status>
where
    T: prost::Name,
{
    Ok(discovery_v3::DiscoveryResponse {
        version_info: version_info.to_string(),
        resources: resources
            .iter()
            .map(misc::pbjson::from_msg)
            .collect::<Result<_, _>>()
            .map_err(|e| tonic::Status::internal(e.to_string()))?,
        type_url: T::type_url(),
        nonce: uuid::Uuid::new_v4().to_string(),
        ..discovery_v3::DiscoveryResponse::default()
    })
}

fn patch_max_direct_response_body_size_bytes(
    route_configuration: &mut route_v3::RouteConfiguration,
) {
    let max_direct_response_body_size_bytes = route_configuration
        .virtual_hosts
        .iter()
        .flat_map(|virtual_host| &virtual_host.routes)
        .filter_map(|route| match &route.action {
            Some(route_v3::route::Action::DirectResponse(route_v3::DirectResponseAction {
                body:
                    Some(core_v3::DataSource {
                        specifier: Some(core_v3::data_source::Specifier::InlineString(body)),
                        ..
                    }),
                ..
            })) => Some(body.len()),
            _ => None,
        })
        .max();
    route_configuration.max_direct_response_body_size_bytes = max_direct_response_body_size_bytes
        .map(|max_direct_response_body_size_bytes| {
            pbjson_types::UInt32Value::from(max_direct_response_body_size_bytes as u32)
        });
}
