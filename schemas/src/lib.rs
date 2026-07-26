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
    #[serde(default)]
    pub metrics: Metrics,
    #[serde(flatten)]
    _extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Metrics {
    #[serde(rename = "vllm:num_requests_running")]
    pub vllm_num_requests_running: Option<u32>,
    #[serde(rename = "vllm:num_requests_waiting")]
    pub vllm_num_requests_waiting: Option<u32>,
}
