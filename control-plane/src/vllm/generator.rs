use tonic_envoy::envoy::config::core::v3 as core_v3;
use tonic_envoy::envoy::config::route::v3 as route_v3;
use tonic_envoy::envoy::r#type::matcher::v3 as matcher_v3;

pub(super) struct Generator<'a> {
    pub(super) models: &'a schemas::List<schemas::Model>,
    pub(super) route_config_name: &'a String,
    pub(super) cluster_name: &'a String,
    pub(super) template_route: Option<&'a route_v3::Route>,
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

        let mut route = self.template_route.cloned().unwrap_or_default();
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
            .request_headers_to_add
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
        Ok(route)
    }

    fn route_fallback(&self) -> anyhow::Result<route_v3::Route> {
        let mut route = self.template_route.cloned().unwrap_or_default();
        let match_ = route.r#match.get_or_insert_default();
        match_.path_specifier = Some(route_v3::route_match::PathSpecifier::Prefix("/".to_owned()));
        let action =
            misc::get_or_insert_default!(&mut route.action, route_v3::route::Action::Route);
        action.cluster_specifier = Some(route_v3::route_action::ClusterSpecifier::Cluster(
            self.cluster_name.clone(),
        ));
        Ok(route)
    }
}
