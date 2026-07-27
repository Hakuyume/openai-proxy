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
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provider {
    pub id: uuid::Uuid,
    pub models: Vec<Model>,
    pub metrics: Metrics,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Metrics {
    #[serde(rename = "vllm:num_requests_running")]
    pub vllm_num_requests_running: Option<u32>,
    #[serde(rename = "vllm:num_requests_waiting")]
    pub vllm_num_requests_waiting: Option<u32>,
}
