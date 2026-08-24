//! Built-in `web_search` backends.
//!
//! Agents always call the same tool. Brave is preferred when its credential
//! is present; otherwise Venice is used when its credential is present.

use std::fmt::Write;
use std::sync::OnceLock;
use std::time::Duration;

use fabro_llm::types::ToolDefinition;

use crate::config::ToolSecrets;
use crate::tool_registry::{RegisteredTool, ToolSource};
use crate::tools::{WEB_SEARCH_TOOL_NAME, required_str};

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const VENICE_SEARCH_URL: &str = "https://api.venice.ai/api/v1/augment/search";
const VENICE_QUERY_MAX_CHARS: usize = 400;
const VENICE_REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const DEFAULT_MAX_RESULTS: u64 = 5;
const MAX_RESULTS: u64 = 20;

#[derive(Clone, Debug)]
pub(crate) enum SearchBackend {
    Brave {
        api_key:    String,
        search_url: String,
    },
    Venice {
        api_key:    String,
        search_url: String,
    },
}

impl SearchBackend {
    #[must_use]
    pub(crate) fn from_secrets(secrets: &ToolSecrets) -> Option<Self> {
        match (
            secrets.brave_search_api_key.as_ref(),
            secrets.venice_api_key.as_ref(),
        ) {
            (Some(api_key), _) => Some(Self::brave(api_key.clone())),
            (None, Some(api_key)) => Some(Self::venice(api_key.clone())),
            (None, None) => None,
        }
    }

    #[must_use]
    pub(crate) fn brave(api_key: String) -> Self {
        Self::Brave {
            api_key,
            search_url: BRAVE_SEARCH_URL.to_string(),
        }
    }

    #[must_use]
    pub(crate) fn venice(api_key: String) -> Self {
        Self::Venice {
            api_key,
            search_url: VENICE_SEARCH_URL.to_string(),
        }
    }

    async fn search(&self, query: &str, max_results: u64) -> Result<String, String> {
        match self {
            Self::Brave {
                api_key,
                search_url,
            } => search_brave(api_key, search_url, query, max_results).await,
            Self::Venice {
                api_key,
                search_url,
            } => {
                if query.chars().count() > VENICE_QUERY_MAX_CHARS {
                    return Err(format!(
                        "query exceeds Venice Search maximum of {VENICE_QUERY_MAX_CHARS} characters"
                    ));
                }
                search_venice(api_key, search_url, query, max_results).await
            }
        }
    }
}

fn search_http_client() -> fabro_http::HttpClient {
    static CLIENT: OnceLock<fabro_http::HttpClient> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            #[cfg(test)]
            {
                fabro_http::test_http_client().expect("Search HTTP client should build")
            }
            #[cfg(not(test))]
            {
                fabro_http::http_client().expect("Search HTTP client should build")
            }
        })
        .clone()
}

async fn search_brave(
    api_key: &str,
    search_url: &str,
    query: &str,
    max_results: u64,
) -> Result<String, String> {
    let count = max_results.min(MAX_RESULTS);
    let resp = search_http_client()
        .get(search_url)
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .query(&[("q", query), ("count", &count.to_string())])
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Brave Search API returned status {}",
            resp.status()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;
    Ok(format_brave_results(&body))
}

async fn search_venice(
    api_key: &str,
    search_url: &str,
    query: &str,
    max_results: u64,
) -> Result<String, String> {
    let limit = max_results.clamp(1, MAX_RESULTS);
    let resp = search_http_client()
        .post(search_url)
        .timeout(VENICE_REQUEST_TIMEOUT)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "query": query,
            "limit": limit,
            "search_provider": "brave",
        }))
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(venice_status_error(status.as_u16(), &resp));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;
    Ok(format_venice_results(&body))
}

fn venice_status_error(status: u16, resp: &fabro_http::Response) -> String {
    let mut message = format!("Venice Search API returned status {status}");
    if status == 402 {
        if let Some(balance) = header_str(resp, "x-venice-balance-usd") {
            let _ = write!(message, " (balance USD {balance})");
        } else if let Some(balance) = header_str(resp, "x-venice-balance-diem") {
            let _ = write!(message, " (balance DIEM {balance})");
        }
    }
    message
}

fn header_str(resp: &fabro_http::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn format_brave_results(body: &serde_json::Value) -> String {
    let results = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(serde_json::Value::as_array);
    format_hits(results.map(|results| {
        results
            .iter()
            .map(|result| SearchHit {
                title:       json_str(result, "title"),
                url:         json_str(result, "url"),
                description: json_str(result, "description"),
                date:        None,
            })
            .collect()
    }))
}

fn format_venice_results(body: &serde_json::Value) -> String {
    let results = body.get("results").and_then(serde_json::Value::as_array);
    format_hits(results.map(|results| {
        results
            .iter()
            .map(|result| SearchHit {
                title:       json_str(result, "title"),
                url:         json_str(result, "url"),
                description: json_str(result, "content"),
                date:        optional_json_str(result, "date"),
            })
            .collect()
    }))
}

struct SearchHit {
    title:       String,
    url:         String,
    description: String,
    date:        Option<String>,
}

fn format_hits(hits: Option<Vec<SearchHit>>) -> String {
    let Some(hits) = hits.filter(|hits| !hits.is_empty()) else {
        return "No results found.".to_string();
    };

    let mut output = String::new();
    for (i, hit) in hits.iter().enumerate() {
        let _ = write!(
            output,
            "{}. {}\n   {}\n   {}\n",
            i + 1,
            hit.title,
            hit.url,
            hit.description
        );
        if let Some(date) = &hit.date {
            let _ = writeln!(output, "   {date}");
        }
        output.push('\n');
    }
    output
}

fn json_str(value: &serde_json::Value, key: &str) -> String {
    optional_json_str(value, key).unwrap_or_else(|| match key {
        "title" => "(no title)".to_string(),
        "url" => "(no url)".to_string(),
        _ => String::new(),
    })
}

fn optional_json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn max_results_arg(args: &serde_json::Value) -> u64 {
    args.get("max_results")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .min(MAX_RESULTS)
}

#[must_use]
pub(crate) fn make_web_search_tool(backend: SearchBackend) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        WEB_SEARCH_TOOL_NAME.into(),
            description: "Search the web when current external information is needed. Returns result titles, URLs, and descriptions; use web_fetch for a specific URL.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "max_results": {"type": "integer", "description": "Maximum number of results (default 5, max 20)"}
                },
                "required": ["query"]
            }),
        },
        executor:   std::sync::Arc::new(move |args, _ctx| {
            let backend = backend.clone();
            Box::pin(async move {
                let query = required_str(&args, "query")?;
                backend.search(query, max_results_arg(&args)).await
            })
        }),
        source:     ToolSource::Native,
    }
}

#[cfg(test)]
#[must_use]
pub(crate) fn make_web_search_tool_with_api_key(api_key: String) -> RegisteredTool {
    make_web_search_tool(SearchBackend::brave(api_key))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::ToolSecrets;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use crate::tool_registry::ToolContext;

    fn secrets(brave: Option<&str>, venice: Option<&str>) -> ToolSecrets {
        ToolSecrets {
            brave_search_api_key: brave.map(str::to_string),
            venice_api_key:       venice.map(str::to_string),
        }
    }

    async fn execute(tool: &RegisteredTool, args: serde_json::Value) -> Result<String, String> {
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        (tool.executor)(args, ToolContext {
            write_locks: None,
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        })
        .await
    }

    #[test]
    fn from_secrets_prefers_brave_when_both_keys_are_present() {
        let backend = SearchBackend::from_secrets(&secrets(Some("brave-key"), Some("venice-key")));
        assert!(matches!(backend, Some(SearchBackend::Brave { .. })));
    }

    #[test]
    fn from_secrets_registers_brave_when_only_brave_key_is_present() {
        let backend = SearchBackend::from_secrets(&secrets(Some("brave-key"), None));
        assert!(matches!(backend, Some(SearchBackend::Brave { .. })));
    }

    #[test]
    fn from_secrets_registers_venice_when_only_venice_key_is_present() {
        let backend = SearchBackend::from_secrets(&secrets(None, Some("venice-key")));
        assert!(matches!(backend, Some(SearchBackend::Venice { .. })));
    }

    #[test]
    fn from_secrets_omits_search_when_both_keys_are_missing() {
        assert!(SearchBackend::from_secrets(&secrets(None, None)).is_none());
    }

    #[test]
    fn format_brave_results_formats_results() {
        let body = serde_json::json!({
            "web": {
                "results": [
                    {"title": "Rust Lang", "url": "https://rust-lang.org", "description": "A systems language"},
                    {"title": "Rust Book", "url": "https://doc.rust-lang.org/book", "description": "The Rust book"}
                ]
            }
        });
        let output = format_brave_results(&body);
        assert!(output.contains("1. Rust Lang"));
        assert!(output.contains("https://rust-lang.org"));
        assert!(output.contains("A systems language"));
        assert!(output.contains("2. Rust Book"));
    }

    #[test]
    fn format_brave_results_no_results() {
        let body = serde_json::json!({"web": {}});
        assert_eq!(format_brave_results(&body), "No results found.");
    }

    #[test]
    fn format_venice_results_includes_date_when_present() {
        let body = serde_json::json!({
            "query": "rust",
            "results": [
                {
                    "title": "Rust Lang",
                    "url": "https://rust-lang.org",
                    "content": "A systems language",
                    "date": "2026-01-02"
                }
            ]
        });
        let output = format_venice_results(&body);
        assert!(output.contains("1. Rust Lang"));
        assert!(output.contains("https://rust-lang.org"));
        assert!(output.contains("A systems language"));
        assert!(output.contains("2026-01-02"));
    }

    #[test]
    fn brave_and_venice_use_the_same_tool_schema() {
        let brave = make_web_search_tool(SearchBackend::brave("key".into()));
        let venice = make_web_search_tool(SearchBackend::venice("key".into()));
        assert_eq!(brave.definition.parameters, venice.definition.parameters);
    }

    #[tokio::test]
    async fn venice_search_posts_augment_search_with_brave_engine() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/augment/search")
                .header("authorization", "Bearer venice-key")
                .json_body(serde_json::json!({
                    "query": "fabro",
                    "limit": 3,
                    "search_provider": "brave"
                }));
            then.status(200).json_body(serde_json::json!({
                "query": "fabro",
                "results": [{
                    "title": "Fabro",
                    "url": "https://docs.fabro.sh",
                    "content": "Agent runtime",
                    "date": "2026-08-21"
                }]
            }));
        });

        let mut backend = SearchBackend::venice("venice-key".into());
        if let SearchBackend::Venice { search_url, .. } = &mut backend {
            *search_url = format!("{}/api/v1/augment/search", server.base_url());
        }
        let tool = make_web_search_tool(backend);
        let output = execute(
            &tool,
            serde_json::json!({
                "query": "fabro",
                "max_results": 3
            }),
        )
        .await
        .expect("venice search should succeed");

        mock.assert();
        assert!(output.contains("1. Fabro"));
        assert!(output.contains("https://docs.fabro.sh"));
        assert!(output.contains("Agent runtime"));
        assert!(output.contains("2026-08-21"));
    }

    #[tokio::test]
    async fn venice_rejects_query_over_400_chars_before_http() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v1/augment/search");
            then.status(200)
                .json_body(serde_json::json!({"results": []}));
        });

        let mut backend = SearchBackend::venice("venice-key".into());
        if let SearchBackend::Venice { search_url, .. } = &mut backend {
            *search_url = format!("{}/api/v1/augment/search", server.base_url());
        }
        let tool = make_web_search_tool(backend);
        let query = "a".repeat(401);
        let err = execute(&tool, serde_json::json!({ "query": query }))
            .await
            .expect_err("overlong query should fail before HTTP");

        mock.assert_calls(0);
        assert!(err.contains("400"));
    }

    #[tokio::test]
    async fn venice_maps_401_402_and_429_to_tool_errors() {
        async fn assert_status(status: u16, header: Option<(&str, &str)>, expected: &str) {
            let server = MockServer::start();
            let mock = match header {
                Some((name, value)) => server.mock(|when, then| {
                    when.method(POST).path("/api/v1/augment/search");
                    then.status(status).header(name, value).body("error");
                }),
                None => server.mock(|when, then| {
                    when.method(POST).path("/api/v1/augment/search");
                    then.status(status).body("error");
                }),
            };
            let mut backend = SearchBackend::venice("venice-key".into());
            if let SearchBackend::Venice { search_url, .. } = &mut backend {
                *search_url = format!("{}/api/v1/augment/search", server.base_url());
            }
            let tool = make_web_search_tool(backend);
            let err = execute(&tool, serde_json::json!({ "query": "fabro" }))
                .await
                .expect_err("status should become a tool error");
            assert_eq!(err, expected);
            mock.assert();
        }

        assert_status(401, None, "Venice Search API returned status 401").await;
        assert_status(
            402,
            Some(("x-venice-balance-usd", "0.12")),
            "Venice Search API returned status 402 (balance USD 0.12)",
        )
        .await;
        assert_status(429, None, "Venice Search API returned status 429").await;
    }

    #[tokio::test]
    async fn brave_search_still_uses_get_and_subscription_token() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/res/v1/web/search")
                .header("x-subscription-token", "brave-key")
                .query_param("q", "rust")
                .query_param("count", "5");
            then.status(200).json_body(serde_json::json!({
                "web": {
                    "results": [{
                        "title": "Rust",
                        "url": "https://rust-lang.org",
                        "description": "A language"
                    }]
                }
            }));
        });

        let mut backend = SearchBackend::brave("brave-key".into());
        if let SearchBackend::Brave { search_url, .. } = &mut backend {
            *search_url = format!("{}/res/v1/web/search", server.base_url());
        }
        let tool = make_web_search_tool(backend);
        let output = execute(&tool, serde_json::json!({ "query": "rust" }))
            .await
            .expect("brave search should succeed");
        mock.assert();
        assert!(output.contains("1. Rust"));
        assert!(output.contains("A language"));
    }
}
