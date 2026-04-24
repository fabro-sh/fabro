#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use fabro_config::bind::Bind;
use fabro_config::{RuntimeDirectory, ServerSettingsBuilder};
use fabro_server::ip_allowlist::{IpAllowlist, IpAllowlistConfig};
use fabro_server::jwt_auth::{AuthMode, resolve_auth_mode_with_lookup};
use fabro_server::serve::{ServeArgs, serve_command};
use fabro_server::server::{RouterOptions, build_router_with_options, create_app_state};
use fabro_util::terminal::Styles;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::helpers::{api, reqwest_status};

const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

async fn start_tcp_server(auth_mode: AuthMode, ip_allowlist: Arc<IpAllowlistConfig>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test TCP listener should bind");
    let addr = listener
        .local_addr()
        .expect("test TCP listener should have a local address");

    let state = create_app_state();
    let router =
        build_router_with_options(state, &auth_mode, ip_allowlist, RouterOptions::default());

    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    addr
}

#[cfg(unix)]
fn build_unix_client(path: &Path) -> fabro_http::HttpClient {
    fabro_http::HttpClientBuilder::new()
        .unix_socket(path)
        .no_proxy()
        .build()
        .expect("Unix test client should build")
}

fn write_test_config(tempdir: &TempDir, settings: &str) -> PathBuf {
    let config_path = tempdir.path().join("settings.toml");
    std::fs::write(&config_path, settings).expect("test settings should write");
    std::fs::write(
        RuntimeDirectory::new(tempdir.path()).env_path(),
        format!("FABRO_DEV_TOKEN={TEST_DEV_TOKEN}\nSESSION_SECRET={TEST_SESSION_SECRET}\n"),
    )
    .expect("test env file should write");
    config_path
}

async fn spawn_served_listener(
    settings: impl AsRef<str>,
) -> (JoinHandle<anyhow::Result<()>>, Bind, TempDir) {
    let tempdir = tempfile::tempdir().expect("temporary server directory should create");
    let config_path = write_test_config(&tempdir, settings.as_ref());
    let styles: &'static Styles = Box::leak(Box::new(Styles::new(false)));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut tx = Some(tx);
    let storage_dir = tempdir.path().to_path_buf();

    let handle = tokio::spawn(async move {
        Box::pin(serve_command(
            ServeArgs {
                bind: None,
                web: false,
                no_web: true,
                model: None,
                provider: None,
                sandbox: None,
                max_concurrent_runs: None,
                config: Some(config_path),
                #[cfg(debug_assertions)]
                watch_web: false,
            },
            styles,
            Some(storage_dir),
            move |bind| {
                let sender = tx.take().expect("server should only report readiness once");
                sender.send(bind.clone()).ok();
                Ok(())
            },
        ))
        .await
    });

    let Ok(bind) = rx.await else {
        let result = handle
            .await
            .expect("server task should not panic before reporting readiness");
        panic!("server should report its bind address: {result:?}");
    };
    (handle, bind, tempdir)
}

async fn wait_for_health(client: &fabro_http::HttpClient, url: &str) {
    for _ in 0..50 {
        if let Ok(response) = client.get(url).send().await {
            let status = response.status();
            if status == 200 {
                return;
            }
        }
        sleep(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for health endpoint at {url}");
}

#[tokio::test]
async fn tcp_accepts_plain_http_requests() {
    let (handle, bind, _tempdir) = spawn_served_listener(
        r#"
_version = 1

[server.listen]
type = "tcp"
address = "127.0.0.1:0"

[server.auth]
methods = ["dev-token"]
"#,
    )
    .await;
    let addr = match bind {
        Bind::Tcp(addr) => addr,
        Bind::Unix(path) => panic!("expected TCP bind, got unix socket at {}", path.display()),
    };
    let client = fabro_http::test_http_client().unwrap();
    wait_for_health(&client, &format!("http://127.0.0.1:{}/health", addr.port())).await;

    let response = client
        .get(format!("http://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .expect("plain HTTP request should succeed");

    reqwest_status(response, StatusCode::OK, "GET /api/v1/runs").await;
    handle.abort();
}

#[tokio::test]
async fn tcp_dev_token_auth_uses_bearer_auth() {
    let resolved = ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
    )
    .expect("test settings should resolve")
    .server;
    let auth_mode = resolve_auth_mode_with_lookup(&resolved, |name| match name {
        "SESSION_SECRET" => Some(TEST_SESSION_SECRET.to_string()),
        "FABRO_DEV_TOKEN" => Some(TEST_DEV_TOKEN.to_string()),
        _ => None,
    })
    .expect("auth mode should resolve");
    let addr = start_tcp_server(auth_mode, Arc::new(IpAllowlistConfig::default())).await;
    let client = fabro_http::test_http_client().unwrap();
    let url = format!("http://127.0.0.1:{}{}", addr.port(), api("/runs"));

    let unauthorized = client.get(&url).send().await.unwrap();
    reqwest_status(unauthorized, StatusCode::UNAUTHORIZED, "GET /api/v1/runs").await;

    let authorized = client
        .get(url)
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .unwrap();
    reqwest_status(authorized, StatusCode::OK, "GET /api/v1/runs").await;
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_accepts_plain_http_requests() {
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("fabro.sock");
    let (handle, bind, _tempdir) = spawn_served_listener(format!(
        r#"
_version = 1

[server.listen]
type = "unix"
path = "{}"

[server.auth]
methods = ["dev-token"]
"#,
        socket_path.display()
    ))
    .await;
    let path = match bind {
        Bind::Unix(path) => path,
        Bind::Tcp(addr) => panic!("expected Unix bind, got TCP address {addr}"),
    };
    let client = build_unix_client(&path);
    wait_for_health(&client, "http://fabro/health").await;

    let response = client
        .get(format!("http://fabro{}", api("/runs")))
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .expect("Unix-socket HTTP request should succeed");

    reqwest_status(response, StatusCode::OK, "GET /api/v1/runs").await;
    handle.abort();
}

#[tokio::test]
async fn tcp_ip_allowlist_uses_connect_info() {
    let addr = start_tcp_server(
        AuthMode::Disabled,
        Arc::new(IpAllowlistConfig {
            allowlist:           IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]),
            trusted_proxy_count: 0,
        }),
    )
    .await;
    let client = fabro_http::test_http_client().unwrap();

    let response = client
        .get(format!("http://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .send()
        .await
        .unwrap();

    reqwest_status(response, StatusCode::FORBIDDEN, "GET /api/v1/runs").await;
}
