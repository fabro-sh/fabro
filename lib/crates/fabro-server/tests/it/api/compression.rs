//! Response compression on the outer router.
//!
//! The compression layer sits at the outermost edge of `build_router`, so
//! these tests exercise it through the full middleware stack rather than in
//! isolation. `/api/v1/openapi.json` is used as the probe response: it is a
//! multi-hundred-KB JSON body, comfortably above the compression size floor.

#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use fabro_server::server::RouterOptions;
use tempfile::TempDir;
use tower::ServiceExt;

use crate::helpers::{api, test_app_state};

/// Router serving an SPA shell comfortably above the compression size floor,
/// through the same fallback service production uses for static assets.
fn spa_router_with_big_index() -> (Router, TempDir) {
    let temp_dir = tempfile::tempdir().expect("SPA fixture tempdir should create");
    std::fs::write(
        temp_dir.path().join("index.html"),
        format!("<!doctype html><title>spa</title>{}", "x".repeat(8192)),
    )
    .expect("SPA fixture index.html should write");
    let app = fabro_server::test_support::build_test_router_with_options(
        test_app_state(),
        RouterOptions {
            web_enabled: true,
            static_asset_root: Some(temp_dir.path().to_path_buf()),
            ..RouterOptions::default()
        },
    );
    (app, temp_dir)
}

async fn serve_on_ephemeral_port(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test TCP listener should bind");
    let addr = listener
        .local_addr()
        .expect("test TCP listener should have a local address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn openapi_request(accept_encoding: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(api("/openapi.json"));
    if let Some(encoding) = accept_encoding {
        builder = builder.header(header::ACCEPT_ENCODING, encoding);
    }
    builder
        .body(Body::empty())
        .expect("openapi request should build")
}

#[tokio::test]
async fn responses_are_gzip_compressed_when_client_accepts_gzip() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let response = app.oneshot(openapi_request(Some("gzip"))).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "large JSON responses should be gzip-compressed when the client asks"
    );
}

#[tokio::test]
async fn responses_are_brotli_compressed_when_client_prefers_br() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let response = app
        .oneshot(openapi_request(Some("gzip, br")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("br"),
        "brotli should win encoding negotiation when offered"
    );
}

#[tokio::test]
async fn spa_assets_are_compressed() {
    // SPA assets are served by the router's fallback service, not a regular
    // route — this test pins that compression covers that path too, since the
    // multi-megabyte JS bundle is the single largest thing the server sends.
    let (app, _temp_dir) = spa_router_with_big_index();
    let request = Request::builder()
        .method("GET")
        .uri("/")
        .header(header::ACCEPT, "text/html")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .expect("spa request should build");
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "SPA shell served through the fallback must be compressed"
    );
}

#[tokio::test]
async fn compression_applies_over_a_real_tcp_connection() {
    // `oneshot` exercises the tower stack directly; this pins the same
    // behavior through hyper's real connection handling, matching how the
    // production server actually serves (`axum::serve`).
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let addr = serve_on_ephemeral_port(app).await;

    let response = fabro_test::test_http_client()
        .get(format!("http://{addr}/api/v1/openapi.json"))
        // Setting the header manually also disables reqwest's transparent
        // decompression, so Content-Encoding stays visible on the response.
        .header(header::ACCEPT_ENCODING.as_str(), "gzip")
        .send()
        .await
        .expect("openapi request should succeed");

    assert_eq!(response.status(), fabro_http::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING.as_str())
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "compression must survive real hyper serving, not just oneshot"
    );
}

#[tokio::test]
async fn spa_assets_compress_over_a_real_tcp_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (app, _temp_dir) = spa_router_with_big_index();
    let addr = serve_on_ephemeral_port(app).await;

    // Raw HTTP/1.1 over the socket: no client-side redirect following or
    // transparent decompression can distort what the server actually sent.
    // Host matches the test state's canonical origin so the canonical-host
    // redirect stays out of the way.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"GET / HTTP/1.1\r\nHost: localhost:3000\r\nAccept: text/html\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let head_len = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response should have a header block");
    let head = String::from_utf8_lossy(&raw[..head_len]).to_lowercase();

    assert!(
        head.starts_with("http/1.1 200"),
        "unexpected response: {head}"
    );
    assert!(
        head.contains("content-encoding: gzip"),
        "fallback-served SPA shell must compress over real TCP; got:\n{head}"
    );
}

#[tokio::test]
async fn responses_stay_identity_encoded_without_accept_encoding() {
    let app = fabro_server::test_support::build_test_router(test_app_state());
    let response = app.oneshot(openapi_request(None)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !response.headers().contains_key(header::CONTENT_ENCODING),
        "clients that don't advertise Accept-Encoding must get identity bodies"
    );
}
