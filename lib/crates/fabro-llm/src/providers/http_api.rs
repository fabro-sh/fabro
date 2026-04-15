use std::collections::HashMap;
use std::time::Duration;

use crate::types::AdapterTimeout;

/// Shared HTTP infrastructure for provider adapters.
///
/// Holds the API key, base URL, reqwest client, default headers, and timeout
/// configuration that every provider needs. Provider-specific fields live on
/// the adapter struct itself.
pub struct HttpApi {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) default_headers: HashMap<String, String>,
    pub(crate) client: fabro_http::HttpClient,
    pub(crate) request_timeout: Option<Duration>,
    pub(crate) stream_read_timeout: Option<Duration>,
}

impl HttpApi {
    fn build_client(timeout: AdapterTimeout) -> fabro_http::HttpClient {
        fabro_http::HttpClientBuilder::new()
            .connect_timeout(Duration::from_secs_f64(timeout.connect))
            .build()
            .expect("LLM HTTP client should build")
    }

    #[must_use]
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let timeout = AdapterTimeout::default();
        let client = Self::build_client(timeout);
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            default_headers: HashMap::new(),
            client,
            request_timeout: timeout.request.map(Duration::from_secs_f64),
            stream_read_timeout: timeout.stream_read.map(Duration::from_secs_f64),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: AdapterTimeout) -> Self {
        self.client = Self::build_client(timeout);
        self.request_timeout = timeout.request.map(Duration::from_secs_f64);
        self.stream_read_timeout = timeout.stream_read.map(Duration::from_secs_f64);
        self
    }

    #[must_use]
    pub fn with_default_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.default_headers = headers;
        self
    }
}
