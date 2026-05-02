use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use fabro_config::ServerSettingsBuilder;
use fabro_server::ip_allowlist::{IpAllowlist, IpAllowlistConfig};
use fabro_server::jwt_auth::{AuthMode, resolve_auth_mode_with_lookup};
use fabro_server::server::{
    RouterOptions, build_router, create_app_state,
    create_app_state_with_runtime_settings_and_options,
};
use tower::ServiceExt;

use crate::helpers::{
    checked_response, response_json, response_status, response_text, settings_from_toml,
};

const DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
const SESSION_SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn dev_token_enabled_auth_mode() -> AuthMode {
    let resolved = ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
    )
    .expect("settings should resolve")
    .server;
    resolve_auth_mode_with_lookup(&resolved, |name| match name {
        "SESSION_SECRET" => Some(SESSION_SECRET.to_string()),
        "FABRO_DEV_TOKEN" => Some(DEV_TOKEN.to_string()),
        _ => None,
    })
    .expect("auth mode should resolve")
}

fn spa_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spa")
}

#[tokio::test]
async fn old_unversioned_routes_return_404() {
    let app = fabro_server::test_support::build_test_router(create_app_state());

    let cases = [(Method::POST, "/completions")];

    for (method, path) in cases {
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        response_status(response, StatusCode::NOT_FOUND, format!("{method} {path}")).await;
    }
}

#[tokio::test]
async fn root_and_health_stay_at_root() {
    let app = fabro_server::test_support::build_test_router_with_options(
        create_app_state(),
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            static_asset_root: Some(spa_fixture_root()),
            ..RouterOptions::default()
        },
    );

    let root_req = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let root_response = app.clone().oneshot(root_req).await.unwrap();
    let root_html = response_text(root_response, StatusCode::OK, "GET /").await;
    assert!(root_html.contains("<div id=\"root\"></div>"));

    let health_req = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let health_response = app.oneshot(health_req).await.unwrap();
    let health_body = response_json(health_response, StatusCode::OK, "GET /health").await;
    assert_eq!(health_body["status"], "ok");
    assert!(
        health_body.get("version").is_none(),
        "health endpoint should not expose version"
    );
}

#[tokio::test]
async fn install_routes_are_absent_in_normal_mode() {
    let app = fabro_server::test_support::build_test_router(create_app_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/install")
                .header("accept", "text/html,application/xhtml+xml")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    response_status(response, StatusCode::NOT_FOUND, "GET /install").await;
}

#[tokio::test]
async fn moved_routes_not_at_root_of_api_prefix() {
    let app = fabro_server::test_support::build_test_router(create_app_state());

    for path in ["/api/v1/health", "/api/v1/"] {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        response_status(response, StatusCode::NOT_FOUND, format!("GET {path}")).await;
    }
}

#[tokio::test]
async fn source_maps_are_not_served() {
    let app = fabro_server::test_support::build_test_router(create_app_state());

    let request = Request::builder()
        .method("GET")
        .uri("/assets/entry-abc123.js.map")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    response_status(
        response,
        StatusCode::NOT_FOUND,
        "GET /assets/entry-abc123.js.map",
    )
    .await;
}

#[tokio::test]
async fn web_enabled_serves_web_only_routes() {
    let auth_mode = dev_token_enabled_auth_mode();
    let app = fabro_server::server::build_router_with_options(
        create_app_state(),
        &auth_mode,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            static_asset_root: Some(spa_fixture_root()),
            ..RouterOptions::default()
        },
    );

    let auth_me_request = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .body(Body::empty())
        .unwrap();
    let auth_me_response = app.clone().oneshot(auth_me_request).await.unwrap();
    response_status(
        auth_me_response,
        StatusCode::UNAUTHORIZED,
        "GET /api/v1/auth/me",
    )
    .await;

    // Browser-style navigation to an SPA route falls back to index.html.
    let setup_request = Request::builder()
        .method("GET")
        .uri("/setup")
        .header("accept", "text/html,application/xhtml+xml")
        .body(Body::empty())
        .unwrap();
    let setup_response = app.clone().oneshot(setup_request).await.unwrap();
    response_status(setup_response, StatusCode::OK, "GET /setup").await;

    // Same path without `Accept: text/html` (e.g. curl, fetch default) is
    // not a browser navigation and must not get the SPA HTML fallback.
    let setup_no_accept = Request::builder()
        .method("GET")
        .uri("/setup")
        .body(Body::empty())
        .unwrap();
    let setup_no_accept_response = app.clone().oneshot(setup_no_accept).await.unwrap();
    response_status(
        setup_no_accept_response,
        StatusCode::NOT_FOUND,
        "GET /setup",
    )
    .await;

    let setup_status_request = Request::builder()
        .method("GET")
        .uri("/api/v1/setup/status")
        .body(Body::empty())
        .unwrap();
    let setup_status_response = app.clone().oneshot(setup_status_request).await.unwrap();
    response_status(
        setup_status_response,
        StatusCode::NOT_FOUND,
        "GET /api/v1/setup/status",
    )
    .await;

    let setup_complete_request = Request::builder()
        .method("GET")
        .uri("/setup/complete")
        .body(Body::empty())
        .unwrap();
    let setup_complete_response = app.clone().oneshot(setup_complete_request).await.unwrap();
    response_status(
        setup_complete_response,
        StatusCode::NOT_FOUND,
        "GET /setup/complete",
    )
    .await;

    let demo_toggle_request = Request::builder()
        .method("POST")
        .uri("/api/v1/demo/toggle")
        .header("authorization", format!("Bearer {DEV_TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"enabled":true}"#))
        .unwrap();
    let demo_toggle_response = checked_response(
        app.clone().oneshot(demo_toggle_request).await.unwrap(),
        StatusCode::OK,
        "POST /api/v1/demo/toggle",
    )
    .await;
    assert!(
        demo_toggle_response.headers().contains_key("set-cookie"),
        "demo toggle should set a cookie"
    );

    // Unregistered /api/* paths must always 404, even for browser-style
    // `Accept: text/html` requests — the SPA fallback never applies to
    // /api/. Guards against API typos silently rendering the UI shell.
    let api_miss = Request::builder()
        .method("GET")
        .uri("/api/v2/nonexistent")
        .header("accept", "text/html")
        .body(Body::empty())
        .unwrap();
    let api_miss_response = app.oneshot(api_miss).await.unwrap();
    response_status(
        api_miss_response,
        StatusCode::NOT_FOUND,
        "GET /api/v2/nonexistent",
    )
    .await;
}

#[tokio::test]
async fn toggle_demo_rejects_unauthenticated_requests() {
    let app = build_router(create_app_state(), dev_token_enabled_auth_mode());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/demo/toggle")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    response_status(
        response,
        StatusCode::UNAUTHORIZED,
        "POST /api/v1/demo/toggle without auth",
    )
    .await;
}

#[tokio::test]
async fn toggle_demo_allows_authenticated_requests() {
    let app = build_router(create_app_state(), dev_token_enabled_auth_mode());

    let response = checked_response(
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/demo/toggle")
                .header("authorization", format!("Bearer {DEV_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK,
        "POST /api/v1/demo/toggle with dev token",
    )
    .await;
    assert!(
        response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("fabro-demo=1")),
        "authenticated demo toggle should set the demo cookie"
    );
}

#[tokio::test]
async fn security_headers_are_applied_to_all_responses() {
    let app = fabro_server::test_support::build_test_router_with_options(
        create_app_state(),
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            static_asset_root: Some(spa_fixture_root()),
            ..RouterOptions::default()
        },
    );

    // Plain HTTP: HSTS must NOT be present.
    let api_response = checked_response(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::OK,
        "GET /health",
    )
    .await;
    let headers = api_response.headers();
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        headers.get("referrer-policy").unwrap(),
        "strict-origin-when-cross-origin"
    );
    assert_eq!(
        headers.get("cross-origin-opener-policy").unwrap(),
        "same-origin"
    );
    assert!(headers.contains_key("permissions-policy"));
    assert_eq!(headers.get("x-xss-protection").unwrap(), "0");
    assert_eq!(headers.get("pragma").unwrap(), "no-cache");
    assert!(
        !headers.contains_key("strict-transport-security"),
        "HSTS must not be emitted over plain HTTP"
    );

    // X-Forwarded-Proto: https signals the request reached an HTTPS edge.
    let https_response = checked_response(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .header("x-forwarded-proto", "https")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::OK,
        "GET /health with x-forwarded-proto=https",
    )
    .await;
    assert_eq!(
        https_response
            .headers()
            .get("strict-transport-security")
            .unwrap(),
        "max-age=63072000; includeSubDomains"
    );

    // SPA fallback path must also get the headers.
    let spa_response = checked_response(
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri("/runs/abc123")
                .header("accept", "text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
        StatusCode::OK,
        "GET /runs/abc123",
    )
    .await;
    assert_eq!(
        spa_response.headers().get("x-frame-options").unwrap(),
        "DENY"
    );
    // Static files set their own cache-control (no-cache for index.html);
    // the middleware default must not stomp on it.
    assert_eq!(
        spa_response.headers().get("cache-control").unwrap(),
        "no-cache"
    );

    // CSP is shipped in Report-Only mode and must cover the sources the
    // embedded SPA actually loads: same-origin scripts, Google Fonts,
    // WASM instantiation (viz-js), data: and blob: images, blob: workers.
    // Inline hashes are optional because the current SPA ships only
    // external module scripts.
    let csp = spa_response
        .headers()
        .get("content-security-policy-report-only")
        .expect("CSP Report-Only header should be emitted")
        .to_str()
        .expect("CSP should be ASCII");
    assert!(csp.contains("default-src 'self'"), "got: {csp}");
    assert!(csp.contains("script-src 'self'"), "got: {csp}");
    assert!(csp.contains("'wasm-unsafe-eval'"), "got: {csp}");
    assert!(
        csp.contains("style-src 'self' https://fonts.googleapis.com 'unsafe-inline'"),
        "got: {csp}"
    );
    assert!(
        csp.contains("font-src 'self' https://fonts.gstatic.com"),
        "got: {csp}"
    );
    assert!(csp.contains("img-src 'self' data: blob:"), "got: {csp}");
    assert!(csp.contains("worker-src 'self' blob:"), "got: {csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "got: {csp}");
    assert!(csp.contains("object-src 'none'"), "got: {csp}");
}

#[tokio::test]
async fn web_disabled_returns_404_for_web_routes_and_keeps_machine_api() {
    let settings = settings_from_toml(
        r"
_version = 1

[server.web]
enabled = false
",
    );
    let app = fabro_server::test_support::build_test_router_with_options(
        create_app_state_with_runtime_settings_and_options(
            settings.server_settings,
            settings.manifest_run_defaults,
            5,
        ),
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            web_enabled: false,
            ..RouterOptions::default()
        },
    );

    for (method, path, body) in [
        ("GET", "/", Body::empty()),
        ("GET", "/setup", Body::empty()),
        ("GET", "/runs/abc", Body::empty()),
        ("GET", "/auth/login/github", Body::empty()),
        ("GET", "/api/v1/auth/me", Body::empty()),
        ("GET", "/api/v1/setup/status", Body::empty()),
        (
            "POST",
            "/api/v1/demo/toggle",
            Body::from(r#"{"enabled":true}"#),
        ),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        response_status(response, StatusCode::NOT_FOUND, format!("{method} {path}")).await;
    }

    let settings_request = Request::builder()
        .method("GET")
        .uri("/api/v1/settings")
        .body(Body::empty())
        .unwrap();
    let settings_response = app.clone().oneshot(settings_request).await.unwrap();
    response_status(settings_response, StatusCode::OK, "GET /api/v1/settings").await;

    let health_request = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let health_response = app.oneshot(health_request).await.unwrap();
    response_status(health_response, StatusCode::OK, "GET /health").await;
}

#[tokio::test]
async fn web_disabled_ignores_demo_header_dispatch() {
    let settings = settings_from_toml(
        r"
_version = 1

[server.web]
enabled = false
",
    );
    let app = fabro_server::test_support::build_test_router_with_options(
        create_app_state_with_runtime_settings_and_options(
            settings.server_settings,
            settings.manifest_run_defaults,
            5,
        ),
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions {
            web_enabled: false,
            ..RouterOptions::default()
        },
    );
    let run_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/runs/{run_id}"))
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    response_status(response, StatusCode::NOT_FOUND, "GET /api/v1/runs/{id}").await;
}

#[tokio::test]
async fn allowlist_blocks_non_allowlisted_api_requests() {
    let app = fabro_server::test_support::build_test_router_with_options(
        create_app_state(),
        Arc::new(IpAllowlistConfig {
            allowlist:           IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]),
            trusted_proxy_count: 0,
        }),
        RouterOptions::default(),
    );

    let response = app
        .oneshot(request_with_connect_info(
            "/api/v1/runs",
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
        ))
        .await
        .unwrap();

    response_status(response, StatusCode::FORBIDDEN, "GET /api/v1/runs").await;
}

#[tokio::test]
async fn allowlist_exempts_health_checks() {
    let app = fabro_server::test_support::build_test_router_with_options(
        create_app_state(),
        Arc::new(IpAllowlistConfig {
            allowlist:           IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]),
            trusted_proxy_count: 0,
        }),
        RouterOptions::default(),
    );

    let response = app
        .oneshot(request_with_connect_info(
            "/health",
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
        ))
        .await
        .unwrap();

    response_status(response, StatusCode::OK, "GET /health").await;
}

fn request_with_connect_info(path: &str, ip: IpAddr) -> Request<Body> {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("routing test request should build");
    let mut request = request;
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 8080)));
    request
}
