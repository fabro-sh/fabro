# Server Readiness Timeout Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent CLI/server readiness and health-check paths from hanging when a socket accepts connections but never returns an HTTP response.

**Architecture:** Use bounded HTTP `/health` probes for managed local server readiness instead of raw socket-connect readiness, and give CLI HTTP transports a finite request timeout. Keep long-lived streams separate so SSE/log-follow behavior is not accidentally capped by short control-plane timeouts.

**Tech Stack:** Rust, Tokio, reqwest via `fabro_http`, `cargo nextest`, pinned nightly rustfmt/clippy.

---

## File Structure

- Modify `lib/crates/fabro-cli/src/server_client.rs`
  - Own CLI transport construction and bounded server health checks.
  - Add regression coverage for silent TCP peers and preserve the existing Unix silent-peer test.
- Modify `lib/crates/fabro-cli/src/commands/server/start.rs`
  - Replace daemon-start raw socket readiness with bounded `/health` readiness for both Unix and TCP binds.
- Modify `lib/crates/fabro-client/src/client.rs`
  - Add a bounded default control-plane HTTP client for explicit `ServerTarget` transports.
  - Do not apply this timeout to caller-supplied transports or event stream bodies.
- Modify `lib/crates/fabro-client/src/target.rs`
  - Apply the same bounded public HTTP client defaults used by OAuth/public-target helper construction.
- Modify `lib/crates/fabro-test/src/lib.rs`
  - Add per-probe timeout to `twin_openai()` readiness polling.

## Task 1: Add CLI HTTP Target Hang Regression

**Files:**
- Modify: `lib/crates/fabro-cli/src/server_client.rs`

- [x] **Step 1: Write a failing TCP silent-peer test**

Add this test in `server_client.rs` `mod tests` near the Unix silent-peer regression:

```rust
#[tokio::test]
async fn http_target_transport_times_out_when_peer_accepts_without_http_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        if let Ok((_stream, _addr)) = listener.accept().await {
            sleep(Duration::from_secs(10)).await;
        }
    });

    let target = ServerTarget::http_url(format!("http://{addr}")).unwrap();
    let client = connect_target_api_client_bundle(&target).await.unwrap();
    let result = time::timeout(Duration::from_millis(750), client.get_health()).await;

    server.abort();
    assert!(
        result.is_ok(),
        "HTTP target health check should return its own timeout error instead of hanging"
    );
    assert!(result.unwrap().is_err());
}
```

- [x] **Step 2: Verify the test fails for the right reason**

Run:

```bash
cargo nextest run -p fabro-cli http_target_transport_times_out_when_peer_accepts_without_http_response --status-level fail --final-status-level fail --show-progress none
```

Expected: FAIL after the outer 750ms timeout because the HTTP request hangs.

## Task 2: Add Bounded CLI Control-Plane HTTP Clients

**Files:**
- Modify: `lib/crates/fabro-cli/src/server_client.rs`
- Modify: `lib/crates/fabro-client/src/client.rs`
- Modify: `lib/crates/fabro-client/src/target.rs`

- [x] **Step 1: Define shared timeout constants**

In `server_client.rs`, keep the existing health-probe constant and add a control-plane request timeout:

```rust
const SERVER_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const CLI_CONTROL_PLANE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
```

In `fabro-client/src/client.rs`, add:

```rust
const DEFAULT_CONTROL_PLANE_REQUEST_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
```

In `fabro-client/src/target.rs`, add the same constant with the same value.

- [x] **Step 2: Apply the timeout only to control-plane client construction**

In `server_client.rs`, update `connect_cli_target_transport()` HTTP and Unix builders:

```rust
let mut builder = cli_http_client_builder().timeout(CLI_CONTROL_PLANE_REQUEST_TIMEOUT);
```

and:

```rust
let mut builder = cli_http_client_builder()
    .unix_socket(path)
    .no_proxy()
    .timeout(CLI_CONTROL_PLANE_REQUEST_TIMEOUT);
```

In `fabro-client/src/client.rs`, update `connect_target_transport()`:

```rust
let mut builder =
    fabro_http::HttpClientBuilder::new().timeout(DEFAULT_CONTROL_PLANE_REQUEST_TIMEOUT);
```

and for Unix:

```rust
let mut builder = fabro_http::HttpClientBuilder::new()
    .unix_socket(path)
    .no_proxy()
    .timeout(DEFAULT_CONTROL_PLANE_REQUEST_TIMEOUT);
```

In `fabro-client/src/target.rs`, update `build_public_http_client()` builders the same way.

- [x] **Step 3: Verify TCP silent-peer regression passes**

Run:

```bash
cargo nextest run -p fabro-cli http_target_transport_times_out_when_peer_accepts_without_http_response --status-level fail --final-status-level fail --show-progress none
```

Expected: PASS.

## Task 3: Replace Daemon Raw Socket Readiness With `/health`

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/server/start.rs`

- [x] **Step 1: Write daemon readiness helpers**

Replace `try_connect()` with a helper that builds a short-timeout HTTP client for the resolved `Bind` and calls `/health`:

```rust
const SERVER_START_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

async fn try_health(bind: &Bind) -> bool {
    let (base_url, client) = match build_health_client(bind) {
        Ok(bundle) => bundle,
        Err(_) => return false,
    };

    let response = time::timeout(
        SERVER_START_HEALTH_PROBE_TIMEOUT,
        client.get(format!("{base_url}/health")).send(),
    )
    .await;

    matches!(response, Ok(Ok(response)) if response.status().is_success())
}

fn build_health_client(bind: &Bind) -> Result<(String, fabro_http::HttpClient)> {
    match bind {
        Bind::Tcp(addr) => Ok((
            format!("http://{addr}"),
            fabro_http::HttpClientBuilder::new()
                .no_proxy()
                .timeout(SERVER_START_HEALTH_PROBE_TIMEOUT)
                .build()?,
        )),
        Bind::Unix(path) => Ok((
            "http://fabro".to_string(),
            fabro_http::HttpClientBuilder::new()
                .unix_socket(path)
                .no_proxy()
                .timeout(SERVER_START_HEALTH_PROBE_TIMEOUT)
                .build()?,
        )),
    }
}
```

Update the startup loop condition from:

```rust
if try_connect(&daemon.bind).await {
```

to:

```rust
if try_health(&daemon.bind).await {
```

Remove unused imports for `TcpStream` and `UnixStream`.

- [x] **Step 2: Add a startup helper unit test for silent TCP readiness**

Add a test in `start.rs` tests:

```rust
#[tokio::test]
async fn try_health_returns_false_for_tcp_peer_that_accepts_without_http_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        if let Ok((_stream, _addr)) = listener.accept().await {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });

    let ready = try_health(&Bind::Tcp(addr)).await;

    server.abort();
    assert!(!ready);
}
```

If clippy flags absolute paths, import `tokio::net::TcpListener` and use the existing `time` import.

- [x] **Step 3: Verify daemon startup tests**

Run:

```bash
cargo nextest run -p fabro-cli 'commands::server::start::tests' --status-level fail --final-status-level fail --show-progress none
cargo nextest run -p fabro-cli 'cmd::server_start' --status-level fail --final-status-level fail --show-progress none
```

Expected: PASS.

## Task 4: Harden Test-Only Twin Readiness Polling

**Files:**
- Modify: `lib/crates/fabro-test/src/lib.rs`

- [x] **Step 1: Bound each `twin_openai()` readiness probe**

In `twin_openai()`, replace:

```rust
if let Ok(resp) = client.get(&healthz_url).send().await {
```

with:

```rust
let response = time::timeout(
    std::time::Duration::from_millis(250),
    client.get(&healthz_url).send(),
)
.await;
if let Ok(Ok(resp)) = response {
```

- [x] **Step 2: Verify test-support coverage**

Run:

```bash
cargo nextest run -p fabro-test twin_openai --status-level fail --final-status-level fail --show-progress none
```

If no test matches that filter, run:

```bash
cargo nextest run -p fabro-test --status-level fail --final-status-level fail --show-progress none
```

Expected: PASS.

## Task 5: Final Verification

**Files:**
- Verify all changed files.

- [x] **Step 1: Run focused regressions**

```bash
cargo nextest run -p fabro-cli unix_socket_probe_times_out_when_peer_accepts_without_http_response --status-level fail --final-status-level fail --show-progress none
cargo nextest run -p fabro-cli http_target_transport_times_out_when_peer_accepts_without_http_response --status-level fail --final-status-level fail --show-progress none
cargo nextest run -p fabro-cli concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up --status-level fail --final-status-level fail --show-progress none
```

Expected: all PASS.

- [x] **Step 2: Run package tests and style checks**

```bash
cargo nextest run -p fabro-cli --status-level fail --final-status-level fail --show-progress none
cargo nextest run -p fabro-test --status-level fail --final-status-level fail --show-progress none
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy -p fabro-cli -p fabro-client -p fabro-test --all-targets -- -D warnings
```

Expected: all PASS. If nextest reports unrelated leaky tests but exit code is 0, record the leaky test names in the final handoff.

## Assumptions

- A 30s control-plane request timeout is acceptable for CLI API calls such as `ps`, `doctor`, `settings`, auth refresh, and model listing.
- Long-lived streams are not created through these default control-plane transports in a way that should be capped at 30s; if a verification failure proves otherwise, move the timeout to only startup/health-specific clients instead of global default target transport.
- The existing Unix silent-peer fix in `server_client.rs` remains part of the baseline change and should not be reverted.
