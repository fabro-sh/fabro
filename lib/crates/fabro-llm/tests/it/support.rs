//! Shared helpers for capturing the wire requests adapters send.

use std::sync::{Arc, Mutex};

use httpmock::prelude::*;

/// One captured wire request, normalized for snapshot stability.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WireCapture {
    pub(crate) method:  String,
    pub(crate) path:    String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body:    serde_json::Value,
}

/// Shared slot the matcher closure writes the captured request into.
pub(crate) type CaptureSlot = Arc<Mutex<Option<WireCapture>>>;

fn capture_request(req: &HttpMockRequest) -> WireCapture {
    let mut headers: Vec<(String, String)> = req
        .headers_vec()
        .iter()
        .map(|(name, value)| {
            let name = name.to_ascii_lowercase();
            let value = match name.as_str() {
                // The mock server binds a random port.
                "host" => "[host]".to_string(),
                // Carries a client version that would churn snapshots.
                "user-agent" => "[user-agent]".to_string(),
                _ => value.clone(),
            };
            (name, value)
        })
        .collect();
    headers.sort();

    WireCapture {
        method: req.method_str().to_string(),
        path: req.uri().path().to_string(),
        headers,
        body: serde_json::from_str(&req.body_string()).expect("request body should be JSON"),
    }
}

/// Mounts a mock on `path` that captures the full request into the returned
/// slot and responds with `response_body`.
pub(crate) fn mount_capture<'a>(
    server: &'a MockServer,
    path: &'static str,
    response_body: serde_json::Value,
) -> (httpmock::Mock<'a>, CaptureSlot) {
    let slot: CaptureSlot = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&slot);
    let mock = server.mock(move |when, then| {
        when.method(POST)
            .path(path)
            .is_true(move |req: &HttpMockRequest| {
                *writer.lock().unwrap() = Some(capture_request(req));
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .json_body(response_body);
    });
    (mock, slot)
}

pub(crate) fn take_capture(slot: &CaptureSlot) -> WireCapture {
    slot.lock()
        .unwrap()
        .take()
        .expect("matcher should have captured the request")
}
