mod aggregated_discovery_service;

use clap::Parser;
use sha2::{Digest, Sha224};
use std::collections::{BTreeMap, BTreeSet};
use std::iter;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tonic_envoy::envoy::config::cluster::v3 as cluster_v3;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::endpoint::v3 as endpoint_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::extensions::upstreams::http::v3 as http_v3;
use tonic_envoy::envoy::r#type::matcher::v3 as matcher_v3;

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

    let (mut ads_reporter, ads_service) = aggregated_discovery_service::service();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            ads_reporter.update(state(
                [("1.2.3.4".parse().unwrap(), 80, "a")],
                "model",
                "local_route",
            )?)?;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    });

    tonic::transport::Server::builder()
        .layer(tower_http::trace::TraceLayer::new_for_grpc())
        .add_service(reflection)
        .add_service(ads_service)
        .serve((Ipv4Addr::UNSPECIFIED, args.port).into())
        .await?;

    Ok(())
}

fn state<I, V, K, N>(
    endpoints: I,
    header_name: K,
    route_configuration_name: N,
) -> Result<aggregated_discovery_service::State, prost::EncodeError>
where
    I: IntoIterator<Item = (IpAddr, u16, V)>,
    V: Into<String>,
    K: Into<String>,
    N: Into<String>,
{
    let mut clusters = BTreeMap::new();
    for (addr, port, header_value) in endpoints {
        let header_value = header_value.into();
        let cluster_name = format!("cluster_{}", hex::encode(Sha224::digest(&header_value)));
        clusters
            .entry(header_value.clone())
            .or_insert_with(|| (cluster_name, BTreeSet::new()))
            .1
            .insert((addr, port));
    }
    let header_name = header_name.into();

    let virtual_host = route_v3::VirtualHost {
        name: "local_service".to_owned(),
        domains: vec!["*".to_owned()],
        routes: iter::once(route_list_models())
            .chain(clusters.iter().map(|(header_value, (cluster_name, _))| {
                route(&header_name, header_value, cluster_name)
            }))
            .collect::<Result<_, _>>()?,
        ..route_v3::VirtualHost::default()
    };

    let route_configuration = route_v3::RouteConfiguration {
        name: route_configuration_name.into(),
        virtual_hosts: vec![virtual_host],
        ..route_v3::RouteConfiguration::default()
    };
    Ok(aggregated_discovery_service::State {
        clusters: clusters
            .values()
            .map(|(cluster_name, endpoints)| cluster(cluster_name, endpoints.iter().copied()))
            .collect::<Result<_, _>>()?,
        route_configurations: vec![route_configuration],
    })
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

fn route_list_models() -> Result<route_v3::Route, prost::EncodeError> {
    Ok(route_v3::Route {
        r#match: Some(route_v3::RouteMatch {
            headers: vec![route_v3::HeaderMatcher {
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
            }],
            path_specifier: Some(route_v3::route_match::PathSpecifier::Path(
                "/v1/models".to_owned(),
            )),
            ..route_v3::RouteMatch::default()
        }),
        response_headers_to_add: vec![core_v3::HeaderValueOption {
            header: Some(core_v3::HeaderValue {
                key: "content-type".to_owned(),
                value: "application/json".to_owned(),
                ..core_v3::HeaderValue::default()
            }),
            ..core_v3::HeaderValueOption::default()
        }],
        action: Some(route_v3::route::Action::DirectResponse(
            route_v3::DirectResponseAction {
                status: http::StatusCode::OK.as_u16() as _,
                body: Some(core_v3::DataSource {
                    specifier: Some(core_v3::data_source::Specifier::InlineString(
                        "hello world".to_owned(),
                    )),
                    ..core_v3::DataSource::default()
                }),
            },
        )),
        ..route_v3::Route::default()
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
