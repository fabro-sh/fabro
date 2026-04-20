use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Request, State as AxumState};
use axum::middleware::{self, Next};
use axum::response::Response as AxumResponse;
use chrono::{Duration as ChronoDuration, Utc};
use fabro_config::{parse_settings_layer, resolve_server_from_file};
use fabro_server::auth::GithubEndpoints;
use fabro_server::ip_allowlist::IpAllowlistConfig;
use fabro_server::jwt_auth::resolve_auth_mode_with_lookup;
use fabro_server::server::{
    RouterOptions, build_router_with_options, create_app_state_with_env_lookup,
};
use fabro_test::{GitHubAppState, TestContext, apply_test_isolation};
use fabro_types::RunAuthMethod;
use hkdf::Hkdf;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::Value;
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use ulid::Ulid;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub(crate) const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";

pub(crate) struct RealAuthHarness {
    pub(crate) api_base_url: String,
    pub(crate) web_base_url: String,
    api_server:              RunningHttpServer,
    web_server:              RunningHttpServer,
    twin:                    fabro_test::TwinGitHub,
    pub(crate) api_requests: ListenerRequestLog,
    pub(crate) web_requests: ListenerRequestLog,
}

impl RealAuthHarness {
    pub(crate) async fn start(github_state: GitHubAppState) -> Self {
        Self::start_with_settings(github_state, &["github"], None).await
    }

    pub(crate) async fn start_with_dev_token(github_state: GitHubAppState) -> Self {
        Self::start_with_settings(github_state, &["github", "dev-token"], Some(TEST_DEV_TOKEN))
            .await
    }

    async fn start_with_settings(
        github_state: GitHubAppState,
        auth_methods: &[&str],
        dev_token: Option<&str>,
    ) -> Self {
        let github_client_id = github_state.oauth_client_id.clone();
        let github_client_secret = github_state.oauth_client_secret.clone();
        let twin = fabro_test::TwinGitHub::start(github_state).await;

        let (api_listener, api_base_url) = bind_listener().await;
        let (web_listener, web_base_url) = bind_listener().await;

        let settings = auth_settings(&web_base_url, &github_client_id, auth_methods);
        let resolved = resolve_server_from_file(&settings).expect("settings should resolve");
        let dev_token = dev_token.map(str::to_string);
        let auth_mode = resolve_auth_mode_with_lookup(&resolved, |name| match name {
            "SESSION_SECRET" => Some(TEST_SESSION_SECRET.to_string()),
            "GITHUB_APP_CLIENT_SECRET" => Some(github_client_secret.clone()),
            "FABRO_DEV_TOKEN" => dev_token.clone(),
            _ => None,
        })
        .expect("auth mode should resolve");
        let state = create_app_state_with_env_lookup(settings, 5, move |name| match name {
            "SESSION_SECRET" => Some(TEST_SESSION_SECRET.to_string()),
            "GITHUB_APP_CLIENT_SECRET" => Some(github_client_secret.clone()),
            "FABRO_DEV_TOKEN" => dev_token.clone(),
            _ => None,
        });
        let github_base = github_base_url(&twin.base_url);
        let router = build_router_with_options(
            state,
            auth_mode,
            Arc::new(IpAllowlistConfig::default()),
            RouterOptions {
                web_enabled:      true,
                github_endpoints: Some(Arc::new(GithubEndpoints::with_bases(
                    github_base.clone(),
                    github_base,
                ))),
            },
        );

        let api_requests = ListenerRequestLog::default();
        let web_requests = ListenerRequestLog::default();
        let api_server = RunningHttpServer::start(api_listener, router.clone(), &api_requests);
        let web_server = RunningHttpServer::start(web_listener, router, &web_requests);
        wait_for_http_ready(&api_base_url).await;
        wait_for_http_ready(&web_base_url).await;
        api_requests.clear();
        web_requests.clear();

        Self {
            api_base_url,
            web_base_url,
            api_server,
            web_server,
            twin,
            api_requests,
            web_requests,
        }
    }

    pub(crate) fn api_target(&self) -> String {
        format!("{}/api/v1", self.api_base_url)
    }

    pub(crate) async fn shutdown(self) {
        self.api_server.shutdown().await;
        self.web_server.shutdown().await;
        self.twin.shutdown().await;
    }
}

pub(crate) async fn complete_login_via_browser(
    context: &TestContext,
    target: &str,
) -> (Output, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut cmd, &context.home_dir);
    cmd.current_dir(&context.temp_dir);
    cmd.args(["auth", "login", "--no-browser", "--server", target]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("auth login should spawn");
    let mut stdout = child
        .stdout
        .take()
        .expect("auth login stdout should be piped");
    let stderr = child
        .stderr
        .take()
        .expect("auth login stderr should be piped");
    let (url_tx, url_rx) = mpsc::channel();
    let stderr_reader = std::thread::spawn(move || read_stderr_and_capture_url(stderr, url_tx));

    let browser_url = wait_for_login_url(&mut child, &mut stdout, &url_rx);
    drive_browser_flow(&browser_url).await;

    let status = child.wait().expect("auth login should exit");
    let mut stdout_bytes = Vec::new();
    stdout
        .read_to_end(&mut stdout_bytes)
        .expect("auth login stdout should be readable");
    let stderr_bytes = stderr_reader.join().expect("stderr reader should join");

    (
        Output {
            status,
            stdout: stdout_bytes,
            stderr: stderr_bytes,
        },
        browser_url,
    )
}

pub(crate) fn run_detached(
    context: &TestContext,
    target: &str,
    workflow: &std::path::Path,
) -> String {
    let output = context
        .run_cmd()
        .args([
            "--server",
            target,
            "--detach",
            "--dry-run",
            "--auto-approve",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("detached run should execute");
    assert!(
        output.status.success(),
        "detached run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(crate) fn saved_auth_entry(context: &TestContext) -> Value {
    auth_file_json(context)["servers"]
        .as_object()
        .and_then(|servers| servers.values().next())
        .cloned()
        .expect("saved auth should contain one server entry")
}

pub(crate) fn expire_saved_access_token(context: &TestContext, issuer: &str) {
    let path = auth_store_path(context);
    let mut file = auth_file_json(context);
    let entry = file["servers"]
        .as_object_mut()
        .and_then(|servers| servers.values_mut().next())
        .and_then(Value::as_object_mut)
        .expect("saved auth should contain one mutable server entry");
    let subject = entry
        .get("subject")
        .and_then(Value::as_object)
        .cloned()
        .expect("saved auth entry should include subject");

    entry.insert(
        "access_token".to_string(),
        Value::String(expired_access_token(issuer, &subject)),
    );
    entry.insert(
        "access_token_expires_at".to_string(),
        Value::String((Utc::now() - ChronoDuration::seconds(30)).to_rfc3339()),
    );

    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&file).expect("saved auth should serialize")
        ),
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", path.display()));
}

pub(crate) fn no_redirect_browser_client() -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("no-redirect browser client should build")
}

#[derive(Clone, Default)]
pub(crate) struct ListenerRequestLog {
    entries: Arc<Mutex<Vec<String>>>,
}

impl ListenerRequestLog {
    pub(crate) fn clear(&self) {
        self.entries
            .lock()
            .expect("request log mutex should lock")
            .clear();
    }

    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.entries
            .lock()
            .expect("request log mutex should lock")
            .iter()
            .any(|entry| entry == needle)
    }
}

struct RunningHttpServer {
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle:      Option<JoinHandle<()>>,
}

impl RunningHttpServer {
    fn start(listener: TcpListener, router: Router, request_log: &ListenerRequestLog) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let router = router.layer(middleware::from_fn_with_state(
            request_log.clone(),
            record_request,
        ));
        let handle = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("test server should serve");
        });

        Self {
            shutdown_tx: Some(shutdown_tx),
            handle:      Some(handle),
        }
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle.await.expect("test server task should join");
        }
    }
}

async fn record_request(
    AxumState(log): AxumState<ListenerRequestLog>,
    req: Request,
    next: Next,
) -> AxumResponse {
    log.entries
        .lock()
        .expect("request log mutex should lock")
        .push(format!("{} {}", req.method(), req.uri().path()));
    next.run(req).await
}

#[derive(serde::Serialize)]
struct TestJwtClaims {
    iss:         String,
    aud:         String,
    sub:         String,
    exp:         u64,
    iat:         u64,
    jti:         String,
    idp_issuer:  String,
    idp_subject: String,
    login:       String,
    name:        String,
    email:       String,
    auth_method: RunAuthMethod,
}

async fn bind_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener
        .local_addr()
        .expect("bound test listener should have an address");
    (listener, format!("http://127.0.0.1:{}", addr.port()))
}

fn auth_settings(
    web_base_url: &str,
    github_client_id: &str,
    auth_methods: &[&str],
) -> fabro_types::settings::SettingsLayer {
    let auth_methods = auth_methods
        .iter()
        .map(|method| format!("\"{method}\""))
        .collect::<Vec<_>>()
        .join(", ");
    parse_settings_layer(&format!(
        r#"
_version = 1

[server.auth]
methods = [{auth_methods}]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.web]
url = "{web_base_url}"

[server.integrations.github]
client_id = "{github_client_id}"
"#
    ))
    .expect("test settings should parse")
}

fn github_base_url(base_url: &str) -> fabro_http::Url {
    fabro_http::Url::parse(&format!("{}/", base_url.trim_end_matches('/')))
        .expect("twin github base URL should parse")
}

async fn drive_browser_flow(browser_url: &str) {
    let client = browser_client();
    let response = client
        .get(browser_url)
        .send()
        .await
        .expect("browser flow request should succeed");
    let status = response.status();
    let final_url = response.url().clone();
    let body = response
        .text()
        .await
        .expect("browser flow response body should be readable");
    if status.is_success() {
        if body.contains("action=\"/auth/cli/resume\"")
            && body.contains("method=\"post\"")
            && body.contains("Authorize CLI login")
        {
            let confirm = client
                .post(final_url.as_str())
                .header(reqwest::header::ORIGIN, url_origin(&final_url))
                .send()
                .await
                .expect("browser confirmation request should succeed");
            let status = confirm.status();
            let body = confirm
                .text()
                .await
                .expect("browser confirmation response body should be readable");
            if status.is_success() {
                return;
            }
            assert!(
                status == reqwest::StatusCode::BAD_REQUEST && body.contains("Login failed:"),
                "browser confirmation failed with {status}\n{body}"
            );
        }
        return;
    }
    assert!(
        status == reqwest::StatusCode::BAD_REQUEST && body.contains("Login failed:"),
        "browser flow failed with {status}\n{body}"
    );
}

fn browser_client() -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .no_proxy()
        .build()
        .expect("browser client should build")
}

fn url_origin(url: &reqwest::Url) -> String {
    let mut origin = format!(
        "{}://{}",
        url.scheme(),
        url.host_str().expect("URL should have a host"),
    );
    if let Some(port) = url.port() {
        origin.push(':');
        origin.push_str(&port.to_string());
    }
    origin
}

async fn wait_for_http_ready(base_url: &str) {
    let client = fabro_test::test_http_client();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match client.get(format!("{base_url}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(_) | Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(response) => panic!("server at {base_url} was not ready: {}", response.status()),
            Err(err) => panic!("server at {base_url} was not ready: {err}"),
        }
    }
}

fn auth_file_json(context: &TestContext) -> Value {
    let path = auth_store_path(context);
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    serde_json::from_str(&contents).expect("saved auth should parse")
}

fn auth_store_path(context: &TestContext) -> std::path::PathBuf {
    context.home_dir.join(".fabro/auth.json")
}

fn expired_access_token(issuer: &str, subject: &serde_json::Map<String, Value>) -> String {
    let key = derived_jwt_key();
    let now = Utc::now();
    let claims = TestJwtClaims {
        iss:         issuer.to_string(),
        aud:         "fabro-cli".to_string(),
        sub:         subject_value(subject, "idp_subject"),
        exp:         (now - ChronoDuration::minutes(10))
            .timestamp()
            .try_into()
            .expect("expired timestamp should be positive"),
        iat:         (now - ChronoDuration::minutes(20))
            .timestamp()
            .try_into()
            .expect("issued-at timestamp should be positive"),
        jti:         Ulid::new().to_string(),
        idp_issuer:  subject_value(subject, "idp_issuer"),
        idp_subject: subject_value(subject, "idp_subject"),
        login:       subject_value(subject, "login"),
        name:        subject_value(subject, "name"),
        email:       subject_value(subject, "email"),
        auth_method: RunAuthMethod::Github,
    };

    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )
    .expect("expired JWT should encode")
}

fn derived_jwt_key() -> [u8; 32] {
    let hkdf = Hkdf::<Sha256>::new(None, TEST_SESSION_SECRET.as_bytes());
    let mut key = [0_u8; 32];
    hkdf.expand(b"fabro-jwt-hs256-v1", &mut key)
        .expect("HKDF should derive the fixed-size JWT key");
    key
}

fn read_stderr_and_capture_url(
    stderr: impl std::io::Read,
    url_tx: mpsc::Sender<String>,
) -> Vec<u8> {
    use std::io::BufRead as _;

    let mut reader = std::io::BufReader::new(stderr);
    let mut stderr_bytes = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .expect("auth login stderr should be readable");
        if read == 0 {
            break;
        }
        let trimmed = String::from_utf8_lossy(&line).trim().to_string();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            let _ = url_tx.send(trimmed);
        }
        stderr_bytes.extend_from_slice(&line);
    }

    stderr_bytes
}

fn subject_value(subject: &serde_json::Map<String, Value>, key: &str) -> String {
    subject
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("saved auth subject should include `{key}`"))
}

fn wait_for_login_url(
    child: &mut std::process::Child,
    stdout: &mut impl std::io::Read,
    url_rx: &mpsc::Receiver<String>,
) -> String {
    let deadline = Instant::now() + LOGIN_TIMEOUT;

    loop {
        match url_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(url) => return url,
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let Some(status) = child.try_wait().expect("auth login should stay alive") {
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("auth login stdout should be readable");
            panic!(
                "auth login exited before printing the browser URL: {status}\nstdout:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
            );
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("auth login should exit after kill");
            let mut stdout_bytes = Vec::new();
            stdout
                .read_to_end(&mut stdout_bytes)
                .expect("auth login stdout should be readable");
            panic!(
                "timed out waiting for auth login browser URL\nstatus: {status}\nstdout:\n{}",
                String::from_utf8_lossy(&stdout_bytes),
            );
        }
    }
}
