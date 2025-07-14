use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "list")]
pub(crate) struct List<T> {
    pub(crate) data: Vec<T>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "object", rename = "model")]
pub(crate) struct Model {
    pub(crate) id: String,
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}
