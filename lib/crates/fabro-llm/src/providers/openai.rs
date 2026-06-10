use std::sync::Arc;

use fabro_model::Catalog;

use crate::attachments::{self, AttachmentPolicy};
use crate::codec::openai_responses::OpenAiResponses;
use crate::codec::{Codec, CodecCtx, CodecParams, EncodedRequest};
use crate::error::Error;
use crate::provider::{
    ProviderAdapter, StreamEventStream, validate_standard_speed, validate_tool_choice,
};
use crate::providers::common::{self as common};
use crate::token_count::{InputTokenCount, InputTokenCountMethod};
use crate::transport::{self, HttpTransport, SseFraming};
use crate::types::{AdapterTimeout, Request, Response, StreamEvent};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Provider adapter for the `OpenAI` Responses API (`/v1/responses`).
///
/// A thin transport shell over the `openai_responses` codec: it owns auth
/// (bearer + org/project headers), base URL, the streaming byte loop, and the
/// route configuration for codex mode. All wire translation lives in the
/// codec.
///
/// Per spec Section 2.7, this adapter uses the Responses API (not Chat
/// Completions) to properly surface reasoning tokens, built-in tools, and
/// server-side state.
pub struct Adapter {
    pub(crate) http: HttpTransport,
    org_id:          Option<String>,
    project_id:      Option<String>,
    provider_name:   String,
    catalog:         Option<Arc<Catalog>>,
    /// When true, always use streaming (required by the Codex endpoint).
    codex_mode:      bool,
}

impl Adapter {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::new_optional_auth(Some(api_key.into()))
    }

    #[must_use]
    pub fn new_optional_auth(api_key: Option<String>) -> Self {
        Self {
            http:          HttpTransport::new_optional(api_key, DEFAULT_BASE_URL),
            org_id:        None,
            project_id:    None,
            provider_name: "openai".to_string(),
            catalog:       None,
            codex_mode:    false,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    #[must_use]
    pub fn with_codex_mode(mut self) -> Self {
        self.codex_mode = true;
        self
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.http.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_org_id(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = Some(org_id.into());
        self
    }

    #[must_use]
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    #[must_use]
    pub fn with_default_headers(self, headers: std::collections::HashMap<String, String>) -> Self {
        Self {
            http: self.http.with_default_headers(headers),
            ..self
        }
    }

    #[must_use]
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    #[must_use]
    pub fn with_timeout(self, timeout: AdapterTimeout) -> Self {
        Self {
            http: self.http.with_timeout(timeout),
            ..self
        }
    }

    /// Per-route dialect knobs for the codec.
    ///
    /// OpenAI has a single auth scheme (bearer + org/project headers), so the
    /// only route variation is codex mode: its encode-side half (param
    /// omission) rides on `CodecParams`; its transport-side half (forced
    /// streaming) is checked directly off `codex_mode` in `complete`.
    fn codec_params(&self) -> CodecParams {
        CodecParams {
            openai_codex: self.codex_mode,
            ..CodecParams::default()
        }
    }

    /// Build the borrowed codec context. `deployment_id` and `params` are
    /// created by the caller so their borrows outlive the context.
    fn codec_ctx<'a>(
        &'a self,
        request: &'a Request,
        deployment_id: &'a str,
        params: &'a CodecParams,
    ) -> CodecCtx<'a> {
        CodecCtx {
            request,
            provider_name: &self.provider_name,
            deployment_id,
            model: common::catalog_model(self.catalog.as_deref(), &request.model),
            params,
        }
    }

    /// Build the canonical request for the codec, resolving file-backed
    /// attachments to inline data first. Borrowed when nothing needs loading.
    async fn resolve_request<'a>(&self, request: &'a Request) -> std::borrow::Cow<'a, Request> {
        // OpenAI loads images inline; audio and documents render as text
        // placeholders in the codec, so they are not loaded here.
        let policy = AttachmentPolicy {
            images:    true,
            documents: false,
            audio:     false,
        };
        attachments::resolve(request, policy).await
    }

    /// Apply the base URL, auth (bearer + org/project headers), and
    /// codec-emitted headers to an encoded request.
    fn build_http_request(&self, encoded: &EncodedRequest) -> fabro_http::RequestBuilder {
        let url = format!("{}{}", self.http.base_url, encoded.endpoint);
        let mut req = self.http.client.post(&url);
        // Apply default_headers first so adapter-specific headers can override
        for (key, value) in &self.http.default_headers {
            req = req.header(key, value);
        }
        if let Some(api_key) = &self.http.api_key {
            req = req.bearer_auth(api_key);
        }
        if let Some(org_id) = &self.org_id {
            req = req.header("OpenAI-Organization", org_id);
        }
        if let Some(project_id) = &self.project_id {
            req = req.header("OpenAI-Project", project_id);
        }
        for (key, value) in &encoded.headers {
            req = req.header(key, value);
        }
        req.json(&encoded.body)
    }

    /// Complete a request by streaming and collecting the final response.
    /// Used for the Codex endpoint which requires `stream: true`.
    async fn complete_via_stream(&self, request: &Request) -> Result<Response, Error> {
        use futures::StreamExt;
        let mut event_stream = self.stream(request).await?;
        let mut last_response: Option<Response> = None;
        while let Some(event) = event_stream.next().await {
            if let StreamEvent::Finish { response, .. } = event? {
                last_response = Some(*response);
                break;
            }
        }
        last_response.ok_or_else(|| Error::Network {
            message: "Stream ended without a finish event".into(),
            source:  None,
        })
    }
}

#[async_trait::async_trait]
impl ProviderAdapter for Adapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn validate_request(&self, request: &Request) -> Result<(), Error> {
        validate_standard_speed(self, request)?;
        if let Some(tc) = &request.tool_choice {
            validate_tool_choice(self, tc)?;
        }
        Ok(())
    }

    async fn count_input_tokens(
        &self,
        request: &Request,
    ) -> Result<Option<InputTokenCount>, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = OpenAiResponses;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = self.codec_params();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let Some(encoded) = codec.encode_count_tokens(&ctx).transpose()? else {
            return Ok(None);
        };

        let mut req = self.build_http_request(&encoded);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        let (body, _headers) =
            transport::send_for_body(req, "input_token_count", &codec, &ctx).await?;
        let input_tokens = codec.decode_count_tokens(&body)?;

        Ok(Some(InputTokenCount {
            input_tokens,
            method: InputTokenCountMethod::ProviderApi,
            provider: self.provider_name.clone(),
            model: request.model.clone(),
            warnings: vec![],
        }))
    }

    async fn complete(&self, request: &Request) -> Result<Response, Error> {
        self.validate_request(request)?;

        // Codex endpoint requires streaming; collect the stream into a
        // response.
        if self.codex_mode {
            return self.complete_via_stream(request).await;
        }

        let resolved = self.resolve_request(request).await;
        let codec = OpenAiResponses;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = self.codec_params();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let encoded = codec.encode(&ctx, false)?;
        let mut req = self.build_http_request(&encoded);
        if let Some(t) = self.http.request_timeout {
            req = req.timeout(t);
        }
        transport::complete_via_http(req, &codec, &ctx).await
    }

    async fn stream(&self, request: &Request) -> Result<StreamEventStream, Error> {
        self.validate_request(request)?;

        let resolved = self.resolve_request(request).await;
        let codec = OpenAiResponses;
        let deployment_id = common::api_model_id(self.catalog.as_deref(), &resolved.model);
        let params = self.codec_params();
        let ctx = self.codec_ctx(&resolved, &deployment_id, &params);

        let encoded = codec.encode(&ctx, true)?;
        transport::stream_via_http(
            self.build_http_request(&encoded),
            &codec,
            &ctx,
            SseFraming::EventBlocks,
            self.http.stream_read_timeout,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use httpmock::prelude::*;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber, subscriber};
    use tracing_subscriber::layer::{Context as SubscriberContext, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};

    use super::*;
    use crate::error::ProviderErrorKind;
    use crate::types::Message;

    fn minimal_request() -> Request {
        Request {
            model:            "gpt-4o".to_string(),
            messages:         vec![Message::user("Hello")],
            provider:         None,
            tools:            None,
            tool_choice:      None,
            response_format:  None,
            temperature:      None,
            top_p:            None,
            max_tokens:       None,
            stop_sequences:   None,
            reasoning_effort: None,
            speed:            None,
            metadata:         None,
            provider_options: None,
        }
    }

    #[derive(Clone, Default)]
    struct CapturedLogEvents(Arc<Mutex<Vec<CapturedLogEvent>>>);

    #[derive(Clone, Debug, Default)]
    struct CapturedLogEvent {
        message: Option<String>,
        fields:  HashMap<String, String>,
    }

    struct CaptureLayer {
        events: CapturedLogEvents,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _ctx: SubscriberContext<'_, S>) {
            let mut visitor = LogFieldVisitor::default();
            event.record(&mut visitor);
            self.events.0.lock().unwrap().push(CapturedLogEvent {
                message: visitor.message,
                fields:  visitor.fields,
            });
        }
    }

    #[derive(Default)]
    struct LogFieldVisitor {
        message: Option<String>,
        fields:  HashMap<String, String>,
    }

    impl LogFieldVisitor {
        fn record_value(&mut self, field: &Field, value: String) {
            if field.name() == "message" {
                self.message = Some(value);
            } else {
                self.fields.insert(field.name().to_string(), value);
            }
        }
    }

    impl Visit for LogFieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.record_value(field, format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.record_value(field, value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.record_value(field, value.to_string());
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.record_value(field, value.to_string());
        }
    }

    #[test]
    fn adapter_with_org_id_sets_field() {
        let adapter = Adapter::new("sk-test").with_org_id("org-123");
        assert_eq!(adapter.org_id.as_deref(), Some("org-123"));
    }

    #[test]
    fn adapter_with_project_id_sets_field() {
        let adapter = Adapter::new("sk-test").with_project_id("proj-456");
        assert_eq!(adapter.project_id.as_deref(), Some("proj-456"));
    }

    #[test]
    fn adapter_with_default_headers_sets_field() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let adapter = Adapter::new("sk-test").with_default_headers(headers);
        assert_eq!(
            adapter
                .http
                .default_headers
                .get("X-Custom")
                .map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn adapter_defaults_have_no_org_project_or_headers() {
        let adapter = Adapter::new("sk-test");
        assert!(adapter.org_id.is_none());
        assert!(adapter.project_id.is_none());
        assert!(adapter.http.default_headers.is_empty());
    }

    #[tokio::test]
    async fn count_input_tokens_posts_count_request_and_parses_response() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/responses/input_tokens");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "object": "response.input_tokens",
                    "input_tokens": 789
                }));
        });
        let adapter = Adapter::new("sk-test").with_base_url(server.base_url());

        let count = adapter
            .count_input_tokens(&minimal_request())
            .await
            .unwrap()
            .expect("openai should count tokens");

        mock.assert();
        assert_eq!(count.input_tokens, 789);
        assert_eq!(count.method, InputTokenCountMethod::ProviderApi);
    }

    #[tokio::test]
    async fn count_input_tokens_logs_operation_on_provider_error() {
        let events = CapturedLogEvents::default();
        let subscriber = Registry::default().with(CaptureLayer {
            events: events.clone(),
        });
        let _guard = subscriber::set_default(subscriber);

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/responses/input_tokens");
            then.status(403)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "error": {
                        "message": "input token counts are not enabled",
                        "type": "permission_error",
                        "code": "insufficient_permissions"
                    }
                }));
        });
        let adapter = Adapter::new("sk-test").with_base_url(server.base_url());

        let err = adapter
            .count_input_tokens(&minimal_request())
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Provider {
            kind: ProviderErrorKind::AccessDenied,
            ..
        }));

        let captured = events.0.lock().unwrap();
        let event = captured
            .iter()
            .find(|event| event.message.as_deref() == Some("Provider returned error"))
            .expect("provider error log should be captured");

        assert_eq!(
            event.fields.get("provider").map(String::as_str),
            Some("openai")
        );
        assert_eq!(event.fields.get("status").map(String::as_str), Some("403"));
        assert_eq!(
            event.fields.get("operation").map(String::as_str),
            Some("input_token_count")
        );
    }

    #[tokio::test]
    async fn count_input_tokens_rejects_wrong_response_object() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/responses/input_tokens");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "object": "other",
                    "input_tokens": 789
                }));
        });
        let adapter = Adapter::new("sk-test").with_base_url(server.base_url());

        let err = adapter
            .count_input_tokens(&minimal_request())
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Configuration { .. }));
    }

    #[tokio::test]
    async fn codex_complete_via_stream_propagates_stream_errors() {
        let server = MockServer::start();
        let sse_body = r#"event: error
data: {"type":"error","error":{"type":"insufficient_quota","code":"insufficient_quota","message":"You exceeded your current quota."}}

"#;

        server.mock(|when, then| {
            when.method(POST).path("/responses");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse_body);
        });

        let adapter = Adapter::new("sk-test")
            .with_base_url(server.base_url())
            .with_codex_mode();

        let err = adapter
            .complete(&minimal_request())
            .await
            .expect_err("codex streaming completion should propagate stream errors");

        match err {
            Error::Provider { kind, detail } => {
                assert_eq!(kind, ProviderErrorKind::QuotaExceeded);
                assert_eq!(detail.error_code.as_deref(), Some("insufficient_quota"));
                assert!(detail.message.contains("exceeded your current quota"));
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }
}
