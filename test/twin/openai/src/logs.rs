use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize)]
pub struct RequestLog {
    pub endpoint: String,
    pub model: String,
    pub stream: bool,
    pub input_text: String,
    pub metadata: Map<String, Value>,
}
