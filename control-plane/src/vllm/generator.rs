use std::time::Duration;
use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::r#type::matcher::v3 as matcher_v3;

pub(super) struct Generator<'a> {
    pub(super) models: &'a schemas::List<schemas::Model>,
    pub(super) route_config_name: &'a String,
    pub(super) cluster_name: &'a String,
    pub(super) timeout: Option<Duration>,
    pub(super) idle_timeout: Option<Duration>,
}

impl Generator<'_> {
    pub(super) fn route_configuration(&self) -> anyhow::Result<route_v3::RouteConfiguration> {
        let mut route_configuration = route_v3::RouteConfiguration {
            name: self.route_config_name.clone(),
            virtual_hosts: vec![self.virtual_host()?],
            ..route_v3::RouteConfiguration::default()
        };
        misc::envoy::patch_max_direct_response_body_size_bytes(&mut route_configuration);
        Ok(route_configuration)
    }

    fn virtual_host(&self) -> anyhow::Result<route_v3::VirtualHost> {
        Ok(route_v3::VirtualHost {
            name: "local_service".to_owned(),
            domains: vec!["*".to_owned()],
            routes: vec![self.route_list_models()?, self.route_fallback()?],
            ..route_v3::VirtualHost::default()
        })
    }

    fn route_list_models(&self) -> anyhow::Result<route_v3::Route> {
        let body = serde_json::to_string(self.models)?;
        Ok(route_v3::Route {
            r#match: Some(route_v3::RouteMatch {
                path_specifier: Some(route_v3::route_match::PathSpecifier::Path(
                    "/v1/models".to_owned(),
                )),
                headers: vec![route_v3::HeaderMatcher {
                    name: ":method".to_owned(),
                    header_match_specifier: Some(
                        route_v3::header_matcher::HeaderMatchSpecifier::StringMatch(
                            matcher_v3::StringMatcher {
                                match_pattern: Some(
                                    matcher_v3::string_matcher::MatchPattern::Exact(
                                        "GET".to_owned(),
                                    ),
                                ),
                                ..matcher_v3::StringMatcher::default()
                            },
                        ),
                    ),
                    ..route_v3::HeaderMatcher::default()
                }],
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
                        specifier: Some(core_v3::data_source::Specifier::InlineString(body)),
                        ..core_v3::DataSource::default()
                    }),
                },
            )),
            ..route_v3::Route::default()
        })
    }

    fn route_fallback(&self) -> anyhow::Result<route_v3::Route> {
        Ok(route_v3::Route {
            r#match: Some(route_v3::RouteMatch {
                path_specifier: Some(route_v3::route_match::PathSpecifier::Prefix("/".to_owned())),
                ..route_v3::RouteMatch::default()
            }),
            action: Some(route_v3::route::Action::Route(route_v3::RouteAction {
                cluster_specifier: Some(route_v3::route_action::ClusterSpecifier::Cluster(
                    self.cluster_name.clone(),
                )),
                timeout: self.timeout.map(TryInto::try_into).transpose()?,
                idle_timeout: self.idle_timeout.map(TryInto::try_into).transpose()?,
                ..route_v3::RouteAction::default()
            })),
            ..route_v3::Route::default()
        })
    }
}
