use crate::config;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) enum Config {
    #[serde(rename = "standard")]
    Standard { bind: config::Bind },
    #[serde(rename = "tunnel")]
    Tunnel {
        #[serde(with = "http_serde::uri")]
        uri: http::Uri,
        authorization: Option<config::Authorization>,
    },
}
