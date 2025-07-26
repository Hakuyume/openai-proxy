use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "list")]
pub struct List<T> {
    pub data: Vec<T>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "model")]
pub struct Model {
    pub id: String,
    pub running: Option<u64>,
    pub pending: Option<u64>,
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
}
