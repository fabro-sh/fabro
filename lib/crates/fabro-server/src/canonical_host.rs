#![expect(
    clippy::disallowed_types,
    reason = "Canonical host middleware parses validated public server.web.url for authority comparison and raw redirect transit; logs use DisplaySafeUrl."
)]

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::uri::PathAndQuery;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use fabro_redact::DisplaySafeUrl;
use tracing::debug;
use url::Url;

use crate::github_webhooks::WEBHOOK_ROUTE;
use crate::server::AppState;

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) state:       Arc<AppState>,
    pub(crate) web_enabled: bool,
}

pub(crate) async fn redirect_middleware(
    State(config): State<Config>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !redirect_applies_to_request(config.web_enabled, request.method(), path) {
        return next.run(request).await;
    }

    let Ok(canonical_origin) = config.state.canonical_origin() else {
        return next.run(request).await;
    };
    let Ok(canonical_url) = Url::parse(&canonical_origin) else {
        return next.run(request).await;
    };
    let Some(request_host) = request.headers().get(header::HOST) else {
        return next.run(request).await;
    };
    if authority_matches(&canonical_url, request_host) {
        return next.run(request).await;
    }

    let path_and_query = request
        .uri()
        .path_and_query()
        .map_or(path, PathAndQuery::as_str);
    let target = format!(
        "{}{}",
        canonical_origin.trim_end_matches('/'),
        path_and_query
    );
    debug!(
        target = %redacted_url_for_log(&target),
        "redirecting to canonical host"
    );
    Redirect::permanent(&target).into_response()
}

fn redirect_applies_to_request(web_enabled: bool, method: &Method, path: &str) -> bool {
    if !web_enabled || redirect_bypass_path(path) {
        return false;
    }

    browser_auth_path(path) || matches!(method, &Method::GET | &Method::HEAD)
}

fn redirect_bypass_path(path: &str) -> bool {
    path == "/health"
        || path == WEBHOOK_ROUTE
        || path == "/api"
        || path.starts_with("/api/")
        || matches!(
            path,
            "/auth/cli/token" | "/auth/cli/refresh" | "/auth/cli/logout"
        )
}

fn browser_auth_path(path: &str) -> bool {
    matches!(
        path,
        "/auth/login/github"
            | "/auth/callback/github"
            | "/auth/login/dev-token"
            | "/auth/logout"
            | "/auth/cli/start"
            | "/auth/cli/resume"
    )
}

fn authority_matches(canonical_url: &Url, request_host: &HeaderValue) -> bool {
    let Some(canonical_authority) = canonical_authority(canonical_url) else {
        return false;
    };
    let Ok(request_host) = request_host.to_str() else {
        return false;
    };
    let request_authority = request_authority(request_host, canonical_url.scheme());

    canonical_authority == request_authority
}

fn canonical_authority(canonical_url: &Url) -> Option<String> {
    let mut authority = normalize_host(canonical_url.host_str()?);
    if let Some(port) = canonical_url.port_or_known_default() {
        if Some(port) != default_port(canonical_url.scheme()) {
            authority.push(':');
            authority.push_str(&port.to_string());
        }
    }
    Some(authority)
}

fn request_authority(request_host: &str, canonical_scheme: &str) -> String {
    let request_host = request_host.trim().to_ascii_lowercase();
    let Some(default_port) = default_port(canonical_scheme) else {
        return request_host;
    };
    let default_port_suffix = format!(":{default_port}");
    request_host
        .strip_suffix(&default_port_suffix)
        .filter(|authority| !authority.is_empty())
        .unwrap_or(&request_host)
        .to_string()
}

fn normalize_host(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]")
    } else {
        host
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn redacted_url_for_log(url: &str) -> String {
    DisplaySafeUrl::parse(url)
        .map_or_else(|_| "<invalid url>".to_string(), |url| url.redacted_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Router, middleware};
    use fabro_config::{RunLayer, ServerSettingsBuilder};
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::github_webhooks::WEBHOOK_ROUTE;
    use crate::server::{self, AppState};

    macro_rules! assert_status {
        ($response:expr, $expected:expr) => {
            fabro_test::assert_axum_status($response, $expected, concat!(file!(), ":", line!()))
        };
    }

    macro_rules! checked_response {
        ($response:expr, $expected:expr) => {
            fabro_test::expect_axum_status($response, $expected, concat!(file!(), ":", line!()))
        };
    }

    fn canonical_url(raw: &str) -> Url {
        Url::parse(raw).expect("canonical URL should parse")
    }

    fn host(value: &'static str) -> axum::http::HeaderValue {
        axum::http::HeaderValue::from_static(value)
    }

    #[test]
    fn authority_comparison_detects_different_loopback_hosts() {
        assert!(!authority_matches(
            &canonical_url("http://127.0.0.1:32276"),
            &host("localhost:32276")
        ));
    }

    #[test]
    fn authority_comparison_matches_default_https_port() {
        assert!(authority_matches(
            &canonical_url("https://example.com"),
            &host("example.com")
        ));
        assert!(authority_matches(
            &canonical_url("https://example.com"),
            &host("example.com:443")
        ));
    }

    #[test]
    fn authority_comparison_rejects_non_default_port() {
        assert!(!authority_matches(
            &canonical_url("https://example.com"),
            &host("example.com:8443")
        ));
    }

    #[test]
    fn authority_comparison_matches_ipv6_authority() {
        assert!(authority_matches(
            &canonical_url("http://[::1]:32276"),
            &host("[::1]:32276")
        ));
    }

    #[test]
    fn authority_comparison_is_case_insensitive() {
        assert!(authority_matches(
            &canonical_url("https://EXAMPLE.com"),
            &host("example.com")
        ));
    }

    fn settings_with_web_url(web_url: &str) -> fabro_types::ServerSettings {
        ServerSettingsBuilder::from_toml(&format!(
            r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[server.web]
url = "{web_url}"
"#
        ))
        .expect("test server settings should resolve")
    }

    fn state_with_web_url(web_url: &str) -> Arc<AppState> {
        server::create_app_state_with_options(
            settings_with_web_url(web_url),
            RunLayer::default(),
            5,
        )
    }

    fn invalid_canonical_origin_state() -> Arc<AppState> {
        server::create_app_state_with_env_lookup(
            settings_with_web_url("{{ env.FABRO_WEB_URL }}"),
            RunLayer::default(),
            5,
            |_| Some("/relative".to_string()),
        )
    }

    fn test_app(state: Arc<AppState>, web_enabled: bool) -> Router {
        Router::new()
            .route("/login", get(|| async { StatusCode::OK }))
            .route("/auth/login/github", get(|| async { StatusCode::OK }))
            .route("/auth/login/dev-token", post(|| async { StatusCode::OK }))
            .route("/auth/logout", post(|| async { StatusCode::OK }))
            .route("/auth/cli/start", get(|| async { StatusCode::OK }))
            .route(
                "/auth/cli/resume",
                get(|| async { StatusCode::OK }).post(|| async { StatusCode::OK }),
            )
            .route("/auth/cli/token", post(|| async { StatusCode::OK }))
            .route("/auth/cli/refresh", post(|| async { StatusCode::OK }))
            .route("/auth/cli/logout", post(|| async { StatusCode::OK }))
            .route("/api/v1/runs", get(|| async { StatusCode::OK }))
            .route("/api/v1/demo/toggle", post(|| async { StatusCode::OK }))
            .route(WEBHOOK_ROUTE, post(|| async { StatusCode::OK }))
            .route("/health", get(|| async { StatusCode::OK }))
            .fallback(|| async { StatusCode::OK.into_response() })
            .layer(middleware::from_fn_with_state(
                Config { state, web_enabled },
                redirect_middleware,
            ))
    }

    async fn request(
        app: Router,
        method: Method,
        uri: &str,
        host: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn assert_redirect(
        app: Router,
        method: Method,
        uri: &str,
        host: &str,
        expected_location: &str,
    ) {
        let response = request(app, method, uri, Some(host)).await;
        let response = checked_response!(response, StatusCode::PERMANENT_REDIRECT).await;
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            expected_location
        );
    }

    #[tokio::test]
    async fn redirects_browser_page_route_to_canonical_host() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);

        assert_redirect(
            app,
            Method::GET,
            "/login",
            "localhost:32276",
            "http://127.0.0.1:32276/login",
        )
        .await;
    }

    #[tokio::test]
    async fn redirects_browser_auth_route_and_preserves_query() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);

        assert_redirect(
            app,
            Method::GET,
            "/auth/login/github?return_to=/runs",
            "localhost:32276",
            "http://127.0.0.1:32276/auth/login/github?return_to=/runs",
        )
        .await;
    }

    #[tokio::test]
    async fn redirects_browser_auth_post_route_to_canonical_host() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);

        assert_redirect(
            app,
            Method::POST,
            "/auth/login/dev-token",
            "localhost:32276",
            "http://127.0.0.1:32276/auth/login/dev-token",
        )
        .await;
    }

    #[tokio::test]
    async fn matching_host_runs_inner_handler() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);
        let response = request(app, Method::GET, "/login", Some("127.0.0.1:32276")).await;

        assert_status!(response, StatusCode::OK).await;
    }

    #[tokio::test]
    async fn bypasses_health_and_api_routes() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);

        let health_response =
            request(app.clone(), Method::GET, "/health", Some("localhost:32276")).await;
        assert_status!(health_response, StatusCode::OK).await;

        let webhook_response = request(
            app.clone(),
            Method::POST,
            WEBHOOK_ROUTE,
            Some("localhost:32276"),
        )
        .await;
        assert_status!(webhook_response, StatusCode::OK).await;

        let runs_response = request(
            app.clone(),
            Method::GET,
            "/api/v1/runs",
            Some("localhost:32276"),
        )
        .await;
        assert_status!(runs_response, StatusCode::OK).await;

        let demo_response = request(
            app,
            Method::POST,
            "/api/v1/demo/toggle",
            Some("localhost:32276"),
        )
        .await;
        assert_status!(demo_response, StatusCode::OK).await;
    }

    #[tokio::test]
    async fn bypasses_cli_token_api_routes_with_authorization() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);
        for path in ["/auth/cli/token", "/auth/cli/refresh", "/auth/cli/logout"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(path)
                        .header(header::HOST, "localhost:32276")
                        .header(header::AUTHORIZATION, "Bearer test")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_status!(response, StatusCode::OK).await;
        }
    }

    #[tokio::test]
    async fn bypasses_when_web_is_disabled() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), false);
        let response = request(app, Method::GET, "/login", Some("localhost:32276")).await;

        assert_status!(response, StatusCode::OK).await;
    }

    #[tokio::test]
    async fn bypasses_when_canonical_origin_is_invalid() {
        let app = test_app(invalid_canonical_origin_state(), true);
        let response = request(app, Method::GET, "/login", Some("localhost:32276")).await;

        assert_status!(response, StatusCode::OK).await;
    }

    #[tokio::test]
    async fn bypasses_when_host_header_is_missing() {
        let app = test_app(state_with_web_url("http://127.0.0.1:32276"), true);
        let response = request(app, Method::GET, "/login", None).await;

        assert_status!(response, StatusCode::OK).await;
    }
}
