use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use fabro_config::parse_settings_layer;
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

    let setup_status_request = Request::builder()
        .method("GET")
        .uri("/api/v1/setup/status")
        .body(Body::empty())
        .unwrap();
    let setup_status_response = app.clone().oneshot(setup_status_request).await.unwrap();
    assert_eq!(setup_status_response.status(), StatusCode::OK);

    let demo_toggle_request = Request::builder()
        .method("POST")
        .uri("/api/v1/demo/toggle")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"enabled":true}"#))
        .unwrap();
    let demo_toggle_response = app.oneshot(demo_toggle_request).await.unwrap();
    assert_eq!(demo_toggle_response.status(), StatusCode::OK);
    assert!(
        demo_toggle_response.headers().contains_key("set-cookie"),
        "demo toggle should set a cookie"
    );
}

#[tokio::test]
async fn web_disabled_returns_404_for_web_routes_and_keeps_machine_api() {
    let settings: SettingsLayer = parse_settings_layer(
        r#"
_version = 1

[server.web]
enabled = false
"#,
    )
    .expect("settings fixture should parse");
    let app = build_router_with_options(
        create_app_state_with_options(settings, 5),
        AuthMode::Disabled,
        RouterOptions { web_enabled: false },
    );

    for (method, path, body) in [
        ("GET", "/", Body::empty()),
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
        r#"
_version = 1

[server.web]
enabled = false
"#,
    )
    .expect("settings fixture should parse");
    let app = build_router_with_options(
        create_app_state_with_options(settings, 5),
        AuthMode::Disabled,
        RouterOptions { web_enabled: false },
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
