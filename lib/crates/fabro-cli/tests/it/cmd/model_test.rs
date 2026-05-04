use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use assert_cmd::Command;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use fabro_test::{fabro_snapshot, test_context};
use httpmock::MockServer;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, oneshot};

fn remove_provider_env(cmd: &mut Command) -> &mut Command {
    cmd.env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("KIMI_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("MINIMAX_API_KEY")
        .env_remove("INCEPTION_API_KEY")
}

fn model_json(id: &str, provider: &str, configured: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "display_name": id,
        "provider": provider,
        "family": "test",
        "aliases": [],
        "limits": {
            "context_window": 131_072,
            "max_output": 4096
        },
        "training": null,
        "knowledge_cutoff": null,
        "features": {
            "tools": true,
            "vision": false,
            "reasoning": false,
            "effort": false
        },
        "costs": {
            "input_cost_per_mtok": 1.0,
            "output_cost_per_mtok": 2.0,
            "cache_input_cost_per_mtok": null
        },
        "estimated_output_tps": 42.0,
        "default": false,
        "configured": configured
    })
}

fn mock_model_list(
    server: &MockServer,
    models: impl IntoIterator<Item = serde_json::Value>,
) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method("GET").path("/api/v1/models");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "data": models.into_iter().collect::<Vec<_>>(),
                "meta": { "has_more": false }
            }));
    })
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Test model availability by sending a simple prompt

    Usage: fabro model test [OPTIONS]

    Options:
          --json                 Output as JSON [env: FABRO_JSON=]
          --server <SERVER>      Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -p, --provider <PROVIDER>  Filter by provider
      -m, --model <MODEL>        Test a specific model
          --no-upgrade-check     Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -j, --jobs <JOBS>          Number of model tests to run concurrently in bulk mode [default: 4]
          --quiet                Suppress non-essential output [env: FABRO_QUIET=]
          --deep                 Run a multi-turn tool-use test (catches reasoning round-trip bugs)
          --verbose              Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                 Print help
    ----- stderr -----
    ");
}

#[test]
fn model_test_unknown_model_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--model", "nonexistent-model-xyz"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Testing nonexistent-model-xyz... done
      × Unknown model: nonexistent-model-xyz
    ");
}

#[test]
fn single_model_skip_exits_nonzero() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test", "--model", "gemini-3.1-pro-preview"]);
    remove_provider_env(&mut cmd);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    MODEL                   PROVIDER  ALIASES     CONTEXT          COST     SPEED  RESULT         
     gemini-3.1-pro-preview  gemini    gemini-pro       1m  $2.0 / $12.0  85 tok/s  not configured
    ----- stderr -----
    Testing gemini-3.1-pro-preview... done
      × 1 model(s) failed
    ");
}

#[test]
fn bulk_skip_exits_zero_and_prints_summary() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    remove_provider_env(&mut cmd);

    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "bulk skip should exit 0:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Skipped"),
        "should report skipped models:\n{stderr}"
    );
}

#[test]
fn json_output_includes_skipped_models() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args([
        "model",
        "test",
        "--model",
        "gemini-3.1-pro-preview",
        "--json",
    ]);
    remove_provider_env(&mut cmd);

    let output = cmd.output().expect("failed to execute model test");
    assert!(
        !output.status.success(),
        "expected single-model skip to exit non-zero:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON output");

    assert_eq!(json["failures"], 1);
    assert_eq!(json["skipped"], 1);
    assert_eq!(json["results"][0]["result"], "skip");
}

#[test]
fn model_test_does_not_announce_unconfigured() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    let list = mock_model_list(&server, [
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("gpt-5.2", "openai", false),
    ]);
    let configured_test = server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "ok"
            }));
    });
    let unconfigured_test = server.mock(|when, then| {
        when.method("POST").path("/api/v1/models/gpt-5.2/test");
        then.status(500)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "errors": [{
                    "status": "500",
                    "title": "should not be called"
                }]
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Testing claude-opus-4-7..."));
    assert!(!stderr.contains("Testing gpt-5.2..."));
    list.assert();
    configured_test.assert();
    unconfigured_test.assert_calls(0);
}

#[test]
fn model_test_skipped_footer_sources_from_listing() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    mock_model_list(&server, [
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("gpt-5.2", "openai", false),
    ]);
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "ok"
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Skipped 1 model(s) (no credentials: OpenAI)"));
}

#[test]
fn model_test_post_list_race_is_a_failure() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    mock_model_list(&server, [model_json("claude-opus-4-7", "anthropic", true)]);
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "skip"
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        !output.status.success(),
        "post-list skip should fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("provider became unconfigured after listing"));
    assert!(stderr.contains("1 model(s) failed"));
    assert!(!stderr.contains("Skipped"));
}

#[test]
fn model_test_json_partitions_skip_and_fail() {
    let context = test_context!();
    let server = MockServer::start();
    context.set_http_target(&server.base_url());
    mock_model_list(&server, [
        model_json("gpt-5.2", "openai", false),
        model_json("claude-opus-4-7", "anthropic", true),
    ]);
    let unconfigured_test = server.mock(|when, then| {
        when.method("POST").path("/api/v1/models/gpt-5.2/test");
        then.status(500);
    });
    server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/models/claude-opus-4-7/test");
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "model_id": "claude-opus-4-7",
                "status": "skip"
            }));
    });

    let mut cmd = context.command();
    cmd.args(["model", "test", "--json"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        !output.status.success(),
        "race failure should make json command fail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    unconfigured_test.assert_calls(0);
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invalid JSON output");
    assert_eq!(json["total"], 2);
    assert_eq!(json["skipped"], 1);
    assert_eq!(json["failures"], 1);
    assert_eq!(json["results"][0]["model"], "gpt-5.2");
    assert_eq!(json["results"][0]["result"], "skip");
    assert_eq!(json["results"][0]["detail"], "not configured");
    assert_eq!(json["results"][1]["model"], "claude-opus-4-7");
    assert_eq!(json["results"][1]["result"], "fail");
    assert_eq!(
        json["results"][1]["error"],
        "provider became unconfigured after listing"
    );
}

#[derive(Clone)]
struct ConcurrentModelServerState {
    models:          Vec<serde_json::Value>,
    gate:            Arc<ConcurrencyGate>,
    response_delays: Arc<HashMap<String, Duration>>,
}

struct ConcurrentModelServer {
    base_url:    String,
    gate:        Arc<ConcurrencyGate>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ConcurrentModelServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .expect("concurrent model test server thread should not panic");
        }
    }
}

struct ConcurrencyGate {
    expected:      usize,
    arrived:       AtomicUsize,
    in_flight:     AtomicUsize,
    max_in_flight: AtomicUsize,
    released:      AtomicBool,
    timed_out:     AtomicBool,
    release:       Semaphore,
}

impl ConcurrencyGate {
    fn new(expected: usize) -> Self {
        Self {
            expected,
            arrived: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            released: AtomicBool::new(expected == 0),
            timed_out: AtomicBool::new(false),
            release: Semaphore::new(0),
        }
    }

    async fn enter(&self) {
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);

        if self.released.load(Ordering::SeqCst) {
            return;
        }

        let arrived = self.arrived.fetch_add(1, Ordering::SeqCst) + 1;
        if arrived >= self.expected {
            if !self.released.swap(true, Ordering::SeqCst) {
                self.release.add_permits(self.expected);
            }
            return;
        }

        let permit = self.release.acquire();
        if self.released.load(Ordering::SeqCst) {
            return;
        }

        if tokio::time::timeout(Duration::from_secs(15), permit)
            .await
            .is_err()
        {
            self.timed_out.store(true, Ordering::SeqCst);
            if !self.released.swap(true, Ordering::SeqCst) {
                self.release.add_permits(self.expected);
            }
        }
    }

    fn exit(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }
}

fn start_concurrent_model_server(
    models: Vec<serde_json::Value>,
    gate_expected: usize,
    response_delays: HashMap<String, Duration>,
) -> ConcurrentModelServer {
    #[expect(
        clippy::disallowed_types,
        reason = "Bind synchronously so we can read the listening port before spawning the \
                  runtime thread; converted to tokio::net::TcpListener inside the runtime."
    )]
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    std_listener
        .set_nonblocking(true)
        .expect("test server listener should be nonblocking");
    let addr: SocketAddr = std_listener
        .local_addr()
        .expect("test server should have addr");
    let gate = Arc::new(ConcurrencyGate::new(gate_expected));
    let state = ConcurrentModelServerState {
        models,
        gate: Arc::clone(&gate),
        response_delays: Arc::new(response_delays),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    #[expect(
        clippy::disallowed_methods,
        reason = "Owns a dedicated OS thread that hosts a fresh Tokio runtime so the test server \
                  is independent of any caller runtime and joinable via Drop."
    )]
    let join_handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime should start");
        runtime.block_on(async move {
            let listener =
                TcpListener::from_std(std_listener).expect("test listener should convert");
            let app = Router::new()
                .route("/api/v1/models", get(concurrent_list_models))
                .route("/api/v1/models/{id}/test", post(concurrent_test_model))
                .with_state(state);
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
    });

    ConcurrentModelServer {
        base_url: format!("http://{addr}"),
        gate,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    }
}

async fn concurrent_list_models(
    State(state): State<ConcurrentModelServerState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": state.models,
        "meta": { "has_more": false }
    }))
}

async fn concurrent_test_model(
    State(state): State<ConcurrentModelServerState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    state.gate.enter().await;
    if let Some(delay) = state.response_delays.get(&id) {
        tokio::time::sleep(*delay).await;
    }
    state.gate.exit();

    Json(serde_json::json!({
        "model_id": id,
        "status": "ok"
    }))
}

#[test]
fn model_test_default_jobs_runs_four_concurrently() {
    let context = test_context!();
    let models = vec![
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("claude-opus-4-6", "anthropic", true),
        model_json("claude-sonnet-4-5", "anthropic", true),
        model_json("claude-sonnet-4-6", "anthropic", true),
        model_json("claude-haiku-4-5", "anthropic", true),
    ];
    let server = start_concurrent_model_server(models, 4, HashMap::new());
    context.set_http_target(&server.base_url);

    let mut cmd = context.command();
    remove_provider_env(&mut cmd);
    cmd.args(["model", "test"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !server.gate.timed_out(),
        "concurrency gate timed out before four requests arrived"
    );
    assert_eq!(
        server.gate.max_in_flight(),
        4,
        "default jobs should run four model tests concurrently before the gate releases"
    );
}

#[test]
fn model_test_explicit_jobs_two_runs_two_concurrently() {
    let context = test_context!();
    let models = vec![
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("claude-opus-4-6", "anthropic", true),
        model_json("claude-sonnet-4-5", "anthropic", true),
        model_json("claude-sonnet-4-6", "anthropic", true),
        model_json("claude-haiku-4-5", "anthropic", true),
    ];
    let server = start_concurrent_model_server(models, 2, HashMap::new());
    context.set_http_target(&server.base_url);

    let mut cmd = context.command();
    remove_provider_env(&mut cmd);
    cmd.args(["model", "test", "--jobs", "2"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !server.gate.timed_out(),
        "concurrency gate timed out before two requests arrived"
    );
    assert_eq!(
        server.gate.max_in_flight(),
        2,
        "--jobs 2 should run two model tests concurrently before the gate releases"
    );
}

#[test]
fn model_test_json_preserves_listing_order_under_concurrency() {
    let context = test_context!();
    let models = vec![
        model_json("claude-opus-4-7", "anthropic", true),
        model_json("claude-opus-4-6", "anthropic", true),
        model_json("claude-sonnet-4-5", "anthropic", true),
        model_json("claude-sonnet-4-6", "anthropic", true),
        model_json("claude-haiku-4-5", "anthropic", true),
    ];
    let response_delays = HashMap::from([
        ("claude-opus-4-7".to_string(), Duration::from_millis(250)),
        ("claude-opus-4-6".to_string(), Duration::from_millis(200)),
        ("claude-sonnet-4-5".to_string(), Duration::from_millis(150)),
        ("claude-sonnet-4-6".to_string(), Duration::from_millis(100)),
        ("claude-haiku-4-5".to_string(), Duration::from_millis(50)),
    ]);
    let server = start_concurrent_model_server(models, 5, response_delays);
    context.set_http_target(&server.base_url);

    let mut cmd = context.command();
    remove_provider_env(&mut cmd);
    cmd.args(["model", "test", "--jobs", "5", "--json"]);
    let output = cmd.output().expect("command should execute");

    assert!(
        output.status.success(),
        "model test should succeed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !server.gate.timed_out(),
        "concurrency gate timed out before five requests arrived"
    );
    assert_eq!(
        server.gate.max_in_flight(),
        5,
        "ordering test should have all five model requests in flight"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invalid JSON output");
    let models = json["results"]
        .as_array()
        .expect("results should be an array")
        .iter()
        .map(|row| row["model"].as_str().expect("model should be a string"))
        .collect::<Vec<_>>();
    assert_eq!(models, vec![
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4-6",
        "claude-haiku-4-5",
    ]);
}
