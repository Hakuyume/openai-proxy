use crate::config;
use std::time::Duration;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Config {
    #[serde(rename = "standard")]
    Standard { bind: config::Bind },
    #[serde(rename = "tunnel")]
    Tunnel {
        #[serde(with = "http_serde::uri")]
        uri: http::Uri,
        authorization: Option<config::Authorization>,
        #[serde(with = "humantime_serde")]
        keep_alive_interval: Duration,
        #[serde(with = "humantime_serde")]
        retry_delay: Duration,
    },
}
