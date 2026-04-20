use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::Request as HttpRequest;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fabro_types::settings::server::{
    IpAllowEntry, ServerIpAllowlistOverrideSettings, ServerIpAllowlistSettings,
};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::warn;

use crate::ApiError;

const GITHUB_META_URL: &str = "https://api.github.com/meta";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpAllowlist {
    entries: Vec<IpNet>,
}

impl IpAllowlist {
    pub fn new(entries: Vec<IpNet>) -> Self {
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        let ip = normalize_ip(*ip);
        self.entries.iter().any(|entry| entry.contains(&ip))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpAllowlistConfig {
    pub allowlist:           IpAllowlist,
    pub trusted_proxy_count: u32,
}

#[derive(Clone)]
pub struct GitHubMetaResolver {
    client:     HttpClient,
    meta_url:   String,
    cache_path: PathBuf,
}

impl GitHubMetaResolver {
    pub fn new(client: HttpClient, meta_url: String, cache_path: PathBuf) -> Self {
        Self {
            client,
            meta_url,
            cache_path,
        }
    }

    pub fn from_cache_dir(cache_dir: &Path) -> Result<Self> {
        Ok(Self::new(
            fabro_http::http_client().context("building GitHub meta HTTP client")?,
            GITHUB_META_URL.to_string(),
            github_meta_cache_path(cache_dir),
        ))
    }

    async fn resolve_hooks(&self) -> Result<Vec<IpNet>> {
        let cached = self.load_cache().await?;
        let mut request = self
            .client
            .get(&self.meta_url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "fabro");

        if let Some(etag) = cached.as_ref().and_then(|cache| cache.etag.as_deref()) {
            request = request.header("If-None-Match", etag);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                return self.cached_hooks_or_error(
                    cached.as_ref(),
                    anyhow!("fetching GitHub /meta: {error}"),
                );
            }
        };
        if response.status() == fabro_http::StatusCode::NOT_MODIFIED {
            let cached = cached.ok_or_else(|| {
                anyhow!("GitHub /meta returned 304 Not Modified but no usable cache was present")
            })?;
            return parse_ip_nets(&cached.hooks);
        }

        if !response.status().is_success() {
            return self.cached_hooks_or_error(
                cached.as_ref(),
                anyhow!("GitHub /meta returned {}", response.status()),
            );
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let payload: GitHubMetaResponse = match response.json().await {
            Ok(payload) => payload,
            Err(error) => {
                return self.cached_hooks_or_error(
                    cached.as_ref(),
                    anyhow!("parsing GitHub /meta response: {error}"),
                );
            }
        };
        let hooks = match parse_ip_nets(&payload.hooks) {
            Ok(hooks) => hooks,
            Err(error) => return self.cached_hooks_or_error(cached.as_ref(), error),
        };
        self.store_cache(&GitHubMetaCache {
            etag,
            hooks: payload.hooks,
        })
        .await?;
        Ok(hooks)
    }

    async fn load_cache(&self) -> Result<Option<GitHubMetaCache>> {
        match fs::read(&self.cache_path).await {
            Ok(contents) => match serde_json::from_slice(&contents) {
                Ok(cache) => Ok(Some(cache)),
                Err(error) => {
                    warn!(
                        path = %self.cache_path.display(),
                        error = %error,
                        "Ignoring invalid GitHub meta cache"
                    );
                    Ok(None)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("reading {}", self.cache_path.display()))
            }
        }
    }

    async fn store_cache(&self, cache: &GitHubMetaCache) -> Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let contents = serde_json::to_vec(cache).context("serializing GitHub meta cache")?;
        fs::write(&self.cache_path, contents)
            .await
            .with_context(|| format!("writing {}", self.cache_path.display()))?;
        Ok(())
    }

    fn cached_hooks_or_error(
        &self,
        cached: Option<&GitHubMetaCache>,
        error: anyhow::Error,
    ) -> Result<Vec<IpNet>> {
        let Some(cached) = cached else {
            return Err(error);
        };

        warn!(
            path = %self.cache_path.display(),
            error = %error,
            "Using cached GitHub meta hooks after refresh failed"
        );
        parse_ip_nets(&cached.hooks)
    }
}

type HttpClient = fabro_http::HttpClient;

#[derive(Debug, Deserialize)]
struct GitHubMetaResponse {
    hooks: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GitHubMetaCache {
    etag:  Option<String>,
    hooks: Vec<String>,
}

pub fn effective_ip_allowlist_settings(
    global: &ServerIpAllowlistSettings,
    overlay: Option<&ServerIpAllowlistOverrideSettings>,
) -> ServerIpAllowlistSettings {
    let Some(overlay) = overlay else {
        return global.clone();
    };

    ServerIpAllowlistSettings {
        entries:             overlay
            .entries
            .clone()
            .unwrap_or_else(|| global.entries.clone()),
        trusted_proxy_count: overlay
            .trusted_proxy_count
            .unwrap_or(global.trusted_proxy_count),
    }
}

pub async fn resolve_ip_allowlist_config(
    global: &ServerIpAllowlistSettings,
    overlay: Option<&ServerIpAllowlistOverrideSettings>,
    github_meta_resolver: &GitHubMetaResolver,
) -> Result<IpAllowlistConfig> {
    let effective = effective_ip_allowlist_settings(global, overlay);
    let allowlist = expand_ip_allow_entries(&effective.entries, github_meta_resolver).await?;

    Ok(IpAllowlistConfig {
        allowlist:           IpAllowlist::new(allowlist),
        trusted_proxy_count: effective.trusted_proxy_count,
    })
}

pub fn extract_client_ip<B>(request: &HttpRequest<B>, trusted_proxy_count: u32) -> Option<IpAddr> {
    if trusted_proxy_count == 0 {
        return request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|connect_info| normalize_ip(connect_info.0.ip()));
    }

    let header = request.headers().get("x-forwarded-for")?.to_str().ok()?;
    let entries = header
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    let trusted_proxy_count = trusted_proxy_count as usize;
    let client_index = entries.len().checked_sub(trusted_proxy_count + 1)?;

    entries[client_index]
        .parse::<IpAddr>()
        .ok()
        .map(normalize_ip)
}

pub async fn ip_allowlist_middleware(
    State(config): State<Arc<IpAllowlistConfig>>,
    request: Request,
    next: Next,
) -> Response {
    if config.allowlist.is_empty() || request.uri().path() == "/health" {
        return next.run(request).await;
    }

    let path = request.uri().path().to_string();
    match extract_client_ip(&request, config.trusted_proxy_count) {
        Some(client_ip) if config.allowlist.contains(&client_ip) => next.run(request).await,
        Some(client_ip) => {
            warn!(client_ip = %client_ip, path = %path, "request rejected: IP not in allowlist");
            ApiError::forbidden().into_response()
        }
        None => {
            warn!(path = %path, "request rejected: IP not in allowlist");
            ApiError::forbidden().into_response()
        }
    }
}

async fn expand_ip_allow_entries(
    entries: &[IpAllowEntry],
    github_meta_resolver: &GitHubMetaResolver,
) -> Result<Vec<IpNet>> {
    let github_hooks = if entries
        .iter()
        .any(|entry| matches!(entry, IpAllowEntry::GitHubMetaHooks))
    {
        github_meta_resolver.resolve_hooks().await?
    } else {
        Vec::new()
    };

    let mut expanded = Vec::new();
    for entry in entries {
        match entry {
            IpAllowEntry::Literal(net) => expanded.push(*net),
            IpAllowEntry::GitHubMetaHooks => expanded.extend(github_hooks.iter().copied()),
        }
    }

    Ok(expanded)
}

fn parse_ip_nets(values: &[String]) -> Result<Vec<IpNet>> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<IpNet>()
                .with_context(|| format!("invalid IP range `{value}` in GitHub /meta hooks"))
        })
        .collect()
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or(IpAddr::V6(address), IpAddr::V4),
    }
}

pub fn github_meta_cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("github-meta-hooks.json")
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests stage IP allowlist fixtures with sync std::fs::write"
)]
mod tests {
    use std::net::Ipv4Addr;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::{Router, middleware};
    use fabro_test::assert_axum_status;
    use httpmock::MockServer;
    use tower::ServiceExt;

    use super::*;

    fn literal(value: &str) -> IpAllowEntry {
        IpAllowEntry::parse_literal(value).unwrap()
    }

    async fn assert_status(response: axum::response::Response, expected: StatusCode) {
        assert_axum_status(response, expected, "assert_status").await;
    }

    #[test]
    fn effective_scope_inherits_global_fields_and_prefers_override_values() {
        let global = ServerIpAllowlistSettings {
            entries:             vec![literal("10.0.0.0/8")],
            trusted_proxy_count: 1,
        };
        let overlay = ServerIpAllowlistOverrideSettings {
            entries:             Some(vec![IpAllowEntry::GitHubMetaHooks]),
            trusted_proxy_count: None,
        };

        let effective = effective_ip_allowlist_settings(&global, Some(&overlay));

        assert_eq!(effective.entries, vec![IpAllowEntry::GitHubMetaHooks]);
        assert_eq!(effective.trusted_proxy_count, 1);
    }

    #[test]
    fn extract_client_ip_uses_connect_info_without_trusted_proxies() {
        let request = Request::builder()
            .uri("/api/v1/runs")
            .body(Body::empty())
            .unwrap();
        let mut request = request;
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 0, 2, 42),
                8080,
            ))));

        assert_eq!(
            extract_client_ip(&request, 0),
            Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 42)))
        );
    }

    #[test]
    fn extract_client_ip_uses_rightmost_minus_trusted_proxy_count_from_x_forwarded_for() {
        let request = Request::builder()
            .uri("/api/v1/runs")
            .header(
                "x-forwarded-for",
                "198.51.100.10, 203.0.113.20, 203.0.113.30",
            )
            .body(Body::empty())
            .unwrap();

        assert_eq!(
            extract_client_ip(&request, 2),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)))
        );
    }

    #[test]
    fn extract_client_ip_fails_closed_when_x_forwarded_for_chain_is_too_short() {
        let request = Request::builder()
            .uri("/api/v1/runs")
            .header("x-forwarded-for", "198.51.100.10")
            .body(Body::empty())
            .unwrap();

        assert_eq!(extract_client_ip(&request, 1), None);
    }

    #[test]
    fn ip_allowlist_matches_ipv4_mapped_ipv6_addresses_against_ipv4_ranges() {
        let allowlist = IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]);

        assert!(allowlist.contains(&"::ffff:10.1.2.3".parse().unwrap()));
    }

    #[test]
    fn github_meta_resolver_uses_storage_cache_dir() {
        let cache_dir = tempfile::tempdir().unwrap();
        let resolver = GitHubMetaResolver::from_cache_dir(cache_dir.path()).unwrap();

        assert_eq!(
            resolver.cache_path,
            cache_dir.path().join("github-meta-hooks.json")
        );
    }

    #[tokio::test]
    async fn resolve_ip_allowlist_config_expands_github_meta_hooks() {
        let mock_server = MockServer::start_async().await;
        mock_server
            .mock_async(|when, then| {
                when.method("GET").path("/meta");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("etag", "\"meta-v1\"")
                    .body(r#"{"hooks":["192.30.252.0/22","185.199.108.0/22"]}"#);
            })
            .await;

        let resolver = GitHubMetaResolver::new(
            fabro_http::test_http_client().unwrap(),
            format!("{}/meta", mock_server.url("")),
            tempfile::tempdir().unwrap().path().join("github-meta.json"),
        );
        let global = ServerIpAllowlistSettings {
            entries:             vec![IpAllowEntry::GitHubMetaHooks],
            trusted_proxy_count: 1,
        };

        let config = resolve_ip_allowlist_config(&global, None, &resolver)
            .await
            .unwrap();

        assert!(config.allowlist.contains(&"192.30.252.45".parse().unwrap()));
        assert!(config.allowlist.contains(&"185.199.109.1".parse().unwrap()));
        assert_eq!(config.trusted_proxy_count, 1);
    }

    #[tokio::test]
    async fn resolve_ip_allowlist_config_reuses_cached_github_meta_on_not_modified() {
        let mock_server = MockServer::start_async().await;
        mock_server
            .mock_async(|when, then| {
                when.method("GET")
                    .path("/meta")
                    .header("if-none-match", "\"meta-v1\"");
                then.status(304);
            })
            .await;

        let cache_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            cache_dir.path().join("github-meta.json"),
            r#"{"etag":"\"meta-v1\"","hooks":["192.30.252.0/22"]}"#,
        )
        .unwrap();

        let resolver = GitHubMetaResolver::new(
            fabro_http::test_http_client().unwrap(),
            format!("{}/meta", mock_server.url("")),
            cache_dir.path().join("github-meta.json"),
        );
        let global = ServerIpAllowlistSettings {
            entries:             vec![IpAllowEntry::GitHubMetaHooks],
            trusted_proxy_count: 0,
        };

        let config = resolve_ip_allowlist_config(&global, None, &resolver)
            .await
            .unwrap();

        assert!(config.allowlist.contains(&"192.30.252.42".parse().unwrap()));
    }

    #[tokio::test]
    async fn resolve_ip_allowlist_config_uses_cached_github_meta_when_github_is_unavailable() {
        let mock_server = MockServer::start_async().await;
        mock_server
            .mock_async(|when, then| {
                when.method("GET").path("/meta");
                then.status(503);
            })
            .await;

        let cache_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            cache_dir.path().join("github-meta.json"),
            r#"{"etag":"\"meta-v1\"","hooks":["192.30.252.0/22"]}"#,
        )
        .unwrap();

        let resolver = GitHubMetaResolver::new(
            fabro_http::test_http_client().unwrap(),
            format!("{}/meta", mock_server.url("")),
            cache_dir.path().join("github-meta.json"),
        );
        let global = ServerIpAllowlistSettings {
            entries:             vec![IpAllowEntry::GitHubMetaHooks],
            trusted_proxy_count: 0,
        };

        let config = resolve_ip_allowlist_config(&global, None, &resolver)
            .await
            .expect("cached GitHub meta hooks should be reused");

        assert!(config.allowlist.contains(&"192.30.252.42".parse().unwrap()));
    }

    #[tokio::test]
    async fn middleware_reads_client_ip_from_x_forwarded_for_when_trusted_proxy_count_is_set() {
        let config = Arc::new(IpAllowlistConfig {
            allowlist:           IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]),
            trusted_proxy_count: 1,
        });
        let app = Router::new()
            .route("/api/v1/runs", get(|| async { StatusCode::OK }))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&config),
                ip_allowlist_middleware,
            ));

        let allowed_request = Request::builder()
            .uri("/api/v1/runs")
            .header("x-forwarded-for", "10.0.0.1, 198.51.100.1")
            .body(Body::empty())
            .unwrap();
        let allowed_response = app.clone().oneshot(allowed_request).await.unwrap();
        assert_status(allowed_response, StatusCode::OK).await;

        let blocked_request = Request::builder()
            .uri("/api/v1/runs")
            .header("x-forwarded-for", "203.0.113.1, 198.51.100.1")
            .body(Body::empty())
            .unwrap();
        let blocked_response = app.oneshot(blocked_request).await.unwrap();
        assert_status(blocked_response, StatusCode::FORBIDDEN).await;
    }

    #[tokio::test]
    async fn middleware_allows_health_and_blocks_non_allowlisted_requests() {
        let config = Arc::new(IpAllowlistConfig {
            allowlist:           IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]),
            trusted_proxy_count: 0,
        });
        let app = Router::new()
            .route("/health", get(|| async { StatusCode::OK }))
            .route("/api/v1/runs", get(|| async { StatusCode::OK }))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&config),
                ip_allowlist_middleware,
            ));

        let health_response = app
            .clone()
            .oneshot(request_with_connect_info(
                "/health",
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
            ))
            .await
            .unwrap();
        assert_status(health_response, StatusCode::OK).await;

        let blocked_response = app
            .oneshot(request_with_connect_info(
                "/api/v1/runs",
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
            ))
            .await
            .unwrap();
        assert_status(blocked_response, StatusCode::FORBIDDEN).await;
    }

    fn request_with_connect_info(path: &str, ip: IpAddr) -> Request<Body> {
        let request = Request::builder().uri(path).body(Body::empty()).unwrap();
        let mut request = request;
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 8080)));
        request
    }
}
