use axum::http::StatusCode;

use crate::openai::models::{ErrorBody, ErrorEnvelope};

use super::plan::ResponsePlan;

#[derive(Clone, Copy, Debug, Default)]
pub struct TransportOptions {
    pub delay_before_headers_ms: u64,
    pub inter_event_delay_ms: u64,
    pub close_after_chunks: Option<usize>,
    pub malformed_sse: bool,
}

#[derive(Clone, Debug)]
pub struct SuccessOutcome {
    pub plan: ResponsePlan,
    pub transport: TransportOptions,
}

#[derive(Clone, Debug)]
pub struct ErrorOutcome {
    pub status: StatusCode,
    pub body: ErrorEnvelope,
    pub retry_after: Option<String>,
    pub delay_before_headers_ms: u64,
}

#[derive(Clone, Debug)]
pub enum ExecutionOutcome {
    Success(SuccessOutcome),
    Error(ErrorOutcome),
    Hang { delay_before_headers_ms: u64 },
}

impl ErrorOutcome {
    pub fn new(
        status: StatusCode,
        message: String,
        error_type: String,
        code: String,
        retry_after: Option<String>,
        delay_before_headers_ms: u64,
    ) -> Self {
        Self {
            status,
            body: ErrorEnvelope {
                error: ErrorBody {
                    message,
                    error_type,
                    param: serde_json::Value::Null,
                    code,
                },
            },
            retry_after,
            delay_before_headers_ms,
        }
    }
}
