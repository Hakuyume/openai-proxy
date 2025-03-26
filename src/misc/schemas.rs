use http::StatusCode;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(tag = "object", rename = "error")]
pub struct Error {
    #[serde(with = "http_serde::status_code")]
    pub code: StatusCode,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "list")]
pub struct List<T> {
    pub data: Vec<T>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "model")]
pub struct Model {
    pub id: String,
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
}
