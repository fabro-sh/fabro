use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use fabro_config::parse_settings_layer;
use fabro_server::ip_allowlist::{IpAllowlist, IpAllowlistConfig};
use fabro_server::jwt_auth::AuthMode;
use fabro_server::server::{
    RouterOptions, build_router, build_router_with_options, create_app_state,
    create_app_state_with_options,
};
use fabro_types::settings::SettingsLayer;
use tower::ServiceExt;

use crate::helpers::body_json;

#[tokio::test]
async fn old_unversioned_routes_return_404() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    let cases = [(Method::POST, "/completions")];

    for (method, path) in cases {
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn root_and_health_stay_at_root() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    let root_req = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let root_response = app.clone().oneshot(root_req).await.unwrap();
    assert_eq!(root_response.status(), StatusCode::OK);
    let root_body = to_bytes(root_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let root_html = String::from_utf8(root_body.to_vec()).unwrap();
    assert!(root_html.contains("<div id=\"root\"></div>"));

    let health_req = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let health_response = app.oneshot(health_req).await.unwrap();
    assert_eq!(health_response.status(), StatusCode::OK);
    let health_body = body_json(health_response.into_body()).await;
    assert_eq!(health_body["status"], "ok");
    assert!(
        health_body.get("version").is_none(),
        "health endpoint should not expose version"
    );
}

#[tokio::test]
async fn install_routes_are_absent_in_normal_mode() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

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
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn moved_routes_not_at_root_of_api_prefix() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    for path in ["/api/v1/health", "/api/v1/"] {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {path}");
    }
}

#[tokio::test]
async fn source_maps_are_not_served() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    let request = Request::builder()
        .method("GET")
        .uri("/assets/entry-abc123.js.map")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn web_enabled_serves_web_only_routes() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    let auth_me_request = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .body(Body::empty())
        .unwrap();
    let auth_me_response = app.clone().oneshot(auth_me_request).await.unwrap();
    assert_eq!(auth_me_response.status(), StatusCode::UNAUTHORIZED);

    // Browser-style navigation to an SPA route falls back to index.html.
    let setup_request = Request::builder()
        .method("GET")
        .uri("/setup")
        .header("accept", "text/html,application/xhtml+xml")
        .body(Body::empty())
        .unwrap();
    let setup_response = app.clone().oneshot(setup_request).await.unwrap();
    assert_eq!(setup_response.status(), StatusCode::OK);

    // Same path without `Accept: text/html` (e.g. curl, fetch default) is
    // not a browser navigation and must not get the SPA HTML fallback.
    let setup_no_accept = Request::builder()
        .method("GET")
        .uri("/setup")
        .body(Body::empty())
        .unwrap();
    let setup_no_accept_response = app.clone().oneshot(setup_no_accept).await.unwrap();
    assert_eq!(setup_no_accept_response.status(), StatusCode::NOT_FOUND);

    let setup_status_request = Request::builder()
        .method("GET")
        .uri("/api/v1/setup/status")
        .body(Body::empty())
        .unwrap();
    let setup_status_response = app.clone().oneshot(setup_status_request).await.unwrap();
    assert_eq!(setup_status_response.status(), StatusCode::NOT_FOUND);

    let setup_complete_request = Request::builder()
        .method("GET")
        .uri("/setup/complete")
        .body(Body::empty())
        .unwrap();
    let setup_complete_response = app.clone().oneshot(setup_complete_request).await.unwrap();
    assert_eq!(setup_complete_response.status(), StatusCode::NOT_FOUND);

    let demo_toggle_request = Request::builder()
        .method("POST")
        .uri("/api/v1/demo/toggle")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"enabled":true}"#))
        .unwrap();
    let demo_toggle_response = app.clone().oneshot(demo_toggle_request).await.unwrap();
    assert_eq!(demo_toggle_response.status(), StatusCode::OK);
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
    assert_eq!(api_miss_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn security_headers_are_applied_to_all_responses() {
    let app = build_router(create_app_state(), AuthMode::Disabled);

    // Plain HTTP: HSTS must NOT be present.
    let api_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
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
    let https_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        https_response
            .headers()
            .get("strict-transport-security")
            .unwrap(),
        "max-age=63072000; includeSubDomains"
    );

    // SPA fallback path must also get the headers.
    let spa_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runs/abc123")
                .header("accept", "text/html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(spa_response.status(), StatusCode::OK);
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
    let csp = spa_response
        .headers()
        .get("content-security-policy-report-only")
        .expect("CSP Report-Only header should be emitted")
        .to_str()
        .expect("CSP should be ASCII");
    assert!(csp.contains("default-src 'self'"), "got: {csp}");
    assert!(csp.contains("'wasm-unsafe-eval'"), "got: {csp}");
    assert!(
        csp.contains("'sha256-"),
        "inline-script hash missing: {csp}"
    );
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
    let settings: SettingsLayer = parse_settings_layer(
        r"
_version = 1

[server.web]
enabled = false
",
    )
    .expect("settings fixture should parse");
    let app = build_router_with_options(
        create_app_state_with_options(settings, 5),
        AuthMode::Disabled,
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
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }

    let settings_request = Request::builder()
        .method("GET")
        .uri("/api/v1/settings")
        .body(Body::empty())
        .unwrap();
    let settings_response = app.clone().oneshot(settings_request).await.unwrap();
    assert_eq!(settings_response.status(), StatusCode::OK);

    let health_request = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let health_response = app.oneshot(health_request).await.unwrap();
    assert_eq!(health_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn web_disabled_ignores_demo_header_dispatch() {
    let settings: SettingsLayer = parse_settings_layer(
        r"
_version = 1

[server.web]
enabled = false
",
    )
    .expect("settings fixture should parse");
    let app = build_router_with_options(
        create_app_state_with_options(settings, 5),
        AuthMode::Disabled,
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
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn allowlist_blocks_non_allowlisted_api_requests() {
    let app = build_router_with_options(
        create_app_state(),
        AuthMode::Disabled,
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

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn allowlist_exempts_health_checks() {
    let app = build_router_with_options(
        create_app_state(),
        AuthMode::Disabled,
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

    assert_eq!(response.status(), StatusCode::OK);
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
