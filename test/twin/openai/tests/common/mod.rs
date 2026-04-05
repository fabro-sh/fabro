#![allow(dead_code, unreachable_pub)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct TestServer {
    pub base_url: String,
    pub client: Client,
    pub auth_client: Client,
    pub bearer_token: String,
}

#[derive(Clone)]
pub struct ApiClient {
    pub base_url: String,
    client: Client,
    bearer_token: Option<String>,
    organization: Option<String>,
    project: Option<String>,
}

pub struct RecordedResponse {
    pub status: reqwest::StatusCode,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct RawStreamResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct TimedStreamResponse {
    pub status: reqwest::StatusCode,
    pub first_event_elapsed: Duration,
    pub chunks: Vec<String>,
}

#[derive(Debug)]
pub struct ParsedSseTranscript {
    pub blocks: Vec<String>,
    pub events: Vec<ParsedSseEvent>,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSseEvent {
    pub event: Option<String>,
    pub data: String,
}

static NEXT_BEARER_TOKEN: AtomicU64 = AtomicU64::new(1);

pub fn test_http_client() -> Result<Client> {
    Client::builder().no_proxy().build().map_err(Into::into)
}

pub async fn spawn_server() -> Result<TestServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let app = twin_openai::build_app_with_config(twin_openai::config::Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        require_auth: true,
        enable_admin: true,
    });

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server should run");
    });

    TestServer::new(format!("http://{addr}"), next_bearer_token())
}

fn next_bearer_token() -> String {
    format!(
        "test-key-{}",
        NEXT_BEARER_TOKEN.fetch_add(1, Ordering::SeqCst)
    )
}

fn authorization_header_value(bearer_token: &str) -> String {
    format!("Bearer {bearer_token}")
}

fn build_authenticated_client(bearer_token: &str) -> Result<Client> {
    Client::builder()
        .no_proxy()
        .default_headers(
            [(
                reqwest::header::AUTHORIZATION,
                authorization_header_value(bearer_token)
                    .parse()
                    .expect("valid header"),
            )]
            .into_iter()
            .collect(),
        )
        .build()
        .map_err(Into::into)
}

impl ApiClient {
    pub fn new(
        base_url: impl Into<String>,
        bearer_token: Option<String>,
        organization: Option<String>,
        project: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            base_url: base_url.into(),
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()?,
            bearer_token,
            organization,
            project,
        })
    }

    pub fn with_client(
        base_url: impl Into<String>,
        client: Client,
        bearer_token: Option<String>,
        organization: Option<String>,
        project: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            client,
            bearer_token,
            organization,
            project,
        }
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> reqwest::Response {
        self.post(path)
            .json(body)
            .send()
            .await
            .expect("request should complete")
    }

    pub async fn post_json_recorded(&self, path: &str, body: &Value) -> RecordedResponse {
        record_response(self.post_json(path, body).await).await
    }

    pub async fn get_recorded(&self, path: &str) -> RecordedResponse {
        record_response(
            self.get(path)
                .send()
                .await
                .expect("request should complete"),
        )
        .await
    }

    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(self.client.post(format!("{}{}", self.base_url, path)))
    }

    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(self.client.get(format!("{}{}", self.base_url, path)))
    }

    fn request(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        }
        if let Some(org) = &self.organization {
            request = request.header("OpenAI-Organization", org);
        }
        if let Some(project) = &self.project {
            request = request.header("OpenAI-Project", project);
        }

        request
    }
}

impl TestServer {
    fn new(base_url: String, bearer_token: String) -> Result<Self> {
        let client = test_http_client()?;
        let auth_client = build_authenticated_client(&bearer_token)?;

        Ok(Self {
            base_url,
            client,
            auth_client,
            bearer_token,
        })
    }

    pub fn authorization_header_value(&self) -> String {
        authorization_header_value(&self.bearer_token)
    }

    pub fn api_client(&self) -> ApiClient {
        ApiClient::with_client(
            self.base_url.clone(),
            self.client.clone(),
            Some(self.bearer_token.clone()),
            None,
            None,
        )
    }

    pub fn fork_namespace(&self) -> Result<TestServer> {
        Self::new(self.base_url.clone(), next_bearer_token())
    }
}

impl TestServer {
    pub async fn post_responses(&self, body: Value) -> reqwest::Response {
        self.auth_client
            .post(format!("{}/v1/responses", self.base_url))
            .json(&body)
            .send()
            .await
            .expect("request should complete")
    }

    pub async fn post_responses_with_headers(
        &self,
        body: Value,
        org: Option<&str>,
        project: Option<&str>,
    ) -> reqwest::Response {
        let mut request = self
            .auth_client
            .post(format!("{}/v1/responses", self.base_url));

        if let Some(org) = org {
            request = request.header("OpenAI-Organization", org);
        }

        if let Some(project) = project {
            request = request.header("OpenAI-Project", project);
        }

        request
            .json(&body)
            .send()
            .await
            .expect("request should complete")
    }

    pub async fn post_responses_stream(&self, body: Value) -> (reqwest::StatusCode, Vec<String>) {
        let response = self
            .auth_client
            .post(format!("{}/v1/responses", self.base_url))
            .json(&body)
            .send()
            .await
            .expect("request should complete");

        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut chunks = Vec::new();

        while let Some(chunk) = stream.next().await {
            chunks.push(
                String::from_utf8(chunk.expect("stream chunk").to_vec()).expect("utf8 stream"),
            );
        }

        (status, chunks)
    }

    pub async fn post_chat(&self, body: Value) -> reqwest::Response {
        self.auth_client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&body)
            .send()
            .await
            .expect("request should complete")
    }

    pub async fn post_chat_stream(&self, body: Value) -> (reqwest::StatusCode, Vec<String>) {
        let response = self.post_chat(body).await;
        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut chunks = Vec::new();

        while let Some(chunk) = stream.next().await {
            chunks.push(
                String::from_utf8(chunk.expect("stream chunk").to_vec()).expect("utf8 stream"),
            );
        }

        (status, chunks)
    }

    pub async fn post_chat_with_auth_header(
        &self,
        body: Value,
        authorization: Option<&str>,
    ) -> reqwest::Response {
        let mut request = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url));

        if let Some(value) = authorization {
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }

        request
            .json(&body)
            .send()
            .await
            .expect("request should complete")
    }

    pub async fn post_responses_stream_timed(&self, body: Value) -> TimedStreamResponse {
        let started = Instant::now();
        let response = self
            .auth_client
            .post(format!("{}/v1/responses", self.base_url))
            .json(&body)
            .send()
            .await
            .expect("request should complete");
        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut chunks = Vec::new();
        let mut first_event_elapsed = Duration::ZERO;

        if let Some(chunk) = stream.next().await {
            chunks.push(
                String::from_utf8(chunk.expect("stream chunk").to_vec()).expect("utf8 stream"),
            );
            first_event_elapsed = started.elapsed();
        }

        while let Some(chunk) = stream.next().await {
            chunks.push(
                String::from_utf8(chunk.expect("stream chunk").to_vec()).expect("utf8 stream"),
            );
        }

        TimedStreamResponse {
            status,
            first_event_elapsed,
            chunks,
        }
    }

    pub async fn post_responses_stream_raw(&self, body: Value) -> RawStreamResponse {
        self.raw_stream_request("/v1/responses", &body).await
    }

    pub async fn post_chat_stream_raw(&self, body: Value) -> RawStreamResponse {
        self.raw_stream_request("/v1/chat/completions", &body).await
    }

    pub async fn enqueue_scenarios(&self, scenarios: Value) {
        let response = self
            .auth_client
            .post(format!("{}/__admin/scenarios", self.base_url))
            .json(&scenarios)
            .send()
            .await
            .expect("admin request should complete");

        assert_eq!(response.status(), 200);
    }

    pub async fn reset(&self) {
        let response = self
            .auth_client
            .post(format!("{}/__admin/reset", self.base_url))
            .send()
            .await
            .expect("admin reset should complete");

        assert_eq!(response.status(), 200);
    }

    pub async fn request_logs(&self) -> Value {
        self.auth_client
            .get(format!("{}/__admin/requests", self.base_url))
            .send()
            .await
            .expect("admin logs should complete")
            .json()
            .await
            .expect("logs json should parse")
    }

    async fn raw_stream_request(&self, path: &str, body: &Value) -> RawStreamResponse {
        let authority = self
            .base_url
            .strip_prefix("http://")
            .expect("http base url");
        let mut stream = tokio::net::TcpStream::connect(authority)
            .await
            .expect("socket should connect");
        let body = serde_json::to_vec(body).expect("json body");
        let authorization = self.authorization_header_value();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: {authorization}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );

        stream
            .write_all(request.as_bytes())
            .await
            .expect("request headers should write");
        stream
            .write_all(&body)
            .await
            .expect("request body should write");
        stream.flush().await.expect("request should flush");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("response should read");

        decode_http_response(&response)
    }
}

pub fn parse_sse_transcript(body: &[u8]) -> Result<ParsedSseTranscript, String> {
    let text = std::str::from_utf8(body).map_err(|_| "sse body was not valid utf-8".to_owned())?;
    let mut blocks = Vec::new();
    let mut events = Vec::new();
    let mut done = false;
    let mut remainder = text;

    while let Some((block, rest)) = remainder.split_once("\n\n") {
        if !block.is_empty() {
            let event = parse_sse_block(block)?;
            if event.data == "[DONE]" {
                done = true;
            }
            blocks.push(block.to_owned());
            events.push(event);
        }
        remainder = rest;
    }

    if !remainder.is_empty() {
        return Err("sse stream ended with an incomplete event".to_owned());
    }

    Ok(ParsedSseTranscript {
        blocks,
        events,
        done,
    })
}

fn parse_sse_block(block: &str) -> Result<ParsedSseEvent, String> {
    let mut event = None;
    let mut data_lines = Vec::new();

    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event: ") {
            event = Some(value.to_owned());
            continue;
        }
        if let Some(value) = line.strip_prefix("data: ") {
            data_lines.push(value.to_owned());
            continue;
        }
        if line.starts_with("id: ") || line.starts_with(':') {
            continue;
        }

        return Err(format!("unsupported sse line: {line}"));
    }

    Ok(ParsedSseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

pub async fn record_response(response: reqwest::Response) -> RecordedResponse {
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let body = response
        .bytes()
        .await
        .expect("response body should read")
        .to_vec();

    RecordedResponse {
        status,
        headers,
        body,
    }
}

fn decode_http_response(response: &[u8]) -> RawStreamResponse {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("http header terminator")
        + 4;
    let head = std::str::from_utf8(&response[..header_end]).expect("header utf8");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse::<u16>()
        .expect("numeric status");
    let mut headers = HashMap::new();

    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').expect("header format");
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        decode_chunked_body(&response[header_end..])
    } else {
        response[header_end..].to_vec()
    };

    RawStreamResponse {
        status,
        headers,
        body,
    }
}

fn decode_chunked_body(encoded: &[u8]) -> Vec<u8> {
    let mut cursor = 0;
    let mut decoded = Vec::new();

    loop {
        let size_end = encoded[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("chunk size terminator")
            + cursor;
        let size = std::str::from_utf8(&encoded[cursor..size_end]).expect("chunk size utf8");
        let size = usize::from_str_radix(size.trim(), 16).expect("hex chunk size");
        cursor = size_end + 2;

        if size == 0 {
            break;
        }

        decoded.extend_from_slice(&encoded[cursor..cursor + size]);
        cursor += size + 2;
    }

    decoded
}
