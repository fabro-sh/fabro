use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use fabro_server::bind::Bind;
use fabro_server::ip_allowlist::{IpAllowlist, IpAllowlistConfig};
use fabro_server::jwt_auth::{AuthMode, ConfiguredAuth};
use fabro_server::serve::{ServeArgs, serve_command};
use fabro_server::server::{
    RouterOptions, build_router, build_router_with_options, create_app_state,
};
use fabro_types::settings::ServerAuthMethod;
use fabro_util::terminal::Styles;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::helpers::api;

const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";

async fn start_tcp_server(auth_mode: AuthMode) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let state = create_app_state();
    let router = build_router(state, auth_mode);

    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    addr
}

async fn start_tcp_server_with_allowlist(
    auth_mode: AuthMode,
    ip_allowlist: Arc<IpAllowlistConfig>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let state = create_app_state();
    let router =
        build_router_with_options(state, auth_mode, ip_allowlist, RouterOptions::default());

    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    addr
}

fn build_client() -> fabro_http::HttpClient {
    fabro_http::test_http_client().unwrap()
}

#[cfg(unix)]
fn build_unix_client(path: &Path) -> fabro_http::HttpClient {
    fabro_http::HttpClientBuilder::new()
        .unix_socket(path)
        .no_proxy()
        .build()
        .unwrap()
}

fn write_test_config(tempdir: &TempDir, settings: &str) -> PathBuf {
    let config_path = tempdir.path().join("settings.toml");
    std::fs::write(&config_path, settings).unwrap();
    std::fs::write(
        tempdir.path().join("server.env"),
        format!("FABRO_DEV_TOKEN={TEST_DEV_TOKEN}\n"),
    )
    .unwrap();
    config_path
}

async fn spawn_served_listener(
    settings: impl AsRef<str>,
) -> (JoinHandle<anyhow::Result<()>>, Bind, TempDir) {
    let tempdir = tempfile::tempdir().unwrap();
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

    let bind = rx.await.expect("server should report its bind address");
    (handle, bind, tempdir)
}

async fn wait_for_tcp_health(addr: SocketAddr) {
    let client = build_client();
    let url = format!("http://127.0.0.1:{}/health", addr.port());

    for _ in 0..50 {
        if let Ok(response) = client.get(&url).send().await {
            if response.status() == 200 {
                return;
            }
        }
        sleep(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for TCP health endpoint at {url}");
}

#[cfg(unix)]
async fn wait_for_unix_health(path: &Path) {
    let client = build_unix_client(path);

    for _ in 0..50 {
        if let Ok(response) = client.get("http://fabro/health").send().await {
            if response.status() == 200 {
                return;
            }
        }
        sleep(Duration::from_millis(10)).await;
    }

    panic!(
        "timed out waiting for Unix socket health endpoint at {}",
        path.display()
    );
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
    wait_for_tcp_health(addr).await;

    let client = build_client();

    let response = client
        .get(format!("http://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .expect("plain HTTP request should succeed");

    assert_eq!(response.status(), 200);
    handle.abort();
}

#[tokio::test]
async fn tcp_dev_token_auth_uses_bearer_auth() {
    let auth_mode = AuthMode::Enabled(ConfiguredAuth {
        methods:   vec![ServerAuthMethod::DevToken],
        dev_token: Some(TEST_DEV_TOKEN.to_string()),
    });
    let addr = start_tcp_server(auth_mode).await;
    let client = build_client();
    let url = format!("http://127.0.0.1:{}{}", addr.port(), api("/runs"));

    let unauthorized = client.get(&url).send().await.unwrap();
    assert_eq!(unauthorized.status(), 401);

    let authorized = client
        .get(url)
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), 200);
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_accepts_plain_http_requests() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let socket_path = std::env::temp_dir().join(format!("fabro-server-it-{unique}.sock"));
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
    wait_for_unix_health(&path).await;

    let response = build_unix_client(&path)
        .get(format!("http://fabro{}", api("/runs")))
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .expect("Unix-socket HTTP request should succeed");

    assert_eq!(response.status(), 200);
    handle.abort();
    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn tcp_ip_allowlist_uses_connect_info() {
    let addr = start_tcp_server_with_allowlist(
        AuthMode::Disabled,
        Arc::new(IpAllowlistConfig {
            allowlist:           IpAllowlist::new(vec!["10.0.0.0/8".parse().unwrap()]),
            trusted_proxy_count: 0,
        }),
    )
    .await;
    let client = build_client();

    let response = client
        .get(format!("http://127.0.0.1:{}{}", addr.port(), api("/runs")))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 403);
}
