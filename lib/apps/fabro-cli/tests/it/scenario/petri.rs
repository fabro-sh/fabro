//! Runs on Petri through a real server and its worker subprocess: the run
//! is created and started with `fabro run --detach`, the server launches
//! `fabro run __run-worker` for it as it does for a legacy run, and the
//! worker executes it through Petri over the HTTP run store.
//!
//! Each test starts its own foreground server on disk storage, because the
//! session's shared daemon keeps its object store in memory and the resume
//! scenario restarts the server. The runs take their host scope through the
//! sandbox-driver host plugin, so the tests skip, and say why, when the
//! executable is not found, unless `FABRO_REQUIRE_SANDBOX_PLUGINS` is set.
//! The plugin's path override crosses into the server and its workers the
//! way `PATH` does.
//!
//! The harness here (the server, the detached run, the status and event
//! reads) is shared with the run-tools scenarios in `petri_tools.rs`.

#![expect(
    clippy::disallowed_methods,
    reason = "these scenarios start a real server subprocess, locate the plugin through the process environment, and poll processes"
)]
#![expect(
    clippy::disallowed_types,
    reason = "the scenarios own the server Child so they can SIGKILL it mid-run"
)]
#![expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]

use std::env;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use fabro_client::ServerTarget;
use fabro_config::{Storage, envfile};
use fabro_petri::SqliteRunStore;
use fabro_petri::checkpoint::CheckpointKey;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::petri::RunKey;
use fabro_static::EnvVars;
use fabro_store::{PlatformRecord, PlatformRecordKind, PlatformRecordStore};
use fabro_test::{
    apply_test_isolation, expect_reqwest_json, fabro_snapshot, isolated_storage_dir, test_context,
};
use fabro_types::RunId;
use fabro_vault::{SecretType, Vault};

use crate::cmd::support::created_run_id;
use crate::support::{TEST_DEV_TOKEN, TEST_SESSION_SECRET, seed_dev_token_auth};

const HOST_PLUGIN: &str = "sandbox-driver-host";
pub(super) const REQUIRE_ENV: &str = "FABRO_REQUIRE_SANDBOX_PLUGINS";
pub(super) const RUN_TIMEOUT: Duration = Duration::from_mins(1);
pub(super) const POLL: Duration = Duration::from_millis(50);

/// The host plugin as Petri's lookup finds it: the override variable, else
/// the executable on `PATH`. `None`, after saying so, when the test should
/// skip; a panic when the environment forbids a skip.
pub(super) fn host_plugin() -> Option<PathBuf> {
    let found = env::var_os(EnvVars::PETRI_SANDBOX_HOST_PLUGIN)
        .map(PathBuf::from)
        .or_else(|| {
            env::split_paths(&env::var_os(EnvVars::PATH)?)
                .map(|dir| dir.join(HOST_PLUGIN))
                .find(|candidate| candidate.is_file())
        });
    if found.is_none() {
        assert!(
            env::var_os(REQUIRE_ENV).is_none(),
            "{REQUIRE_ENV} is set, but {HOST_PLUGIN} is not on PATH and {} is unset",
            EnvVars::PETRI_SANDBOX_HOST_PLUGIN
        );
        eprintln!(
            "skipping: {HOST_PLUGIN} is not on PATH and {} is unset",
            EnvVars::PETRI_SANDBOX_HOST_PLUGIN
        );
    }
    found
}

/// A foreground server on its own disk storage, dev-token auth, started
/// from the compiled `fabro` binary. Dropping it kills the process.
pub(super) struct RunningServer {
    child:                   Option<Child>,
    home_root:               tempfile::TempDir,
    _storage_root:           tempfile::TempDir,
    pub(super) storage_dir:  PathBuf,
    config_path:             PathBuf,
    port:                    u16,
    pub(super) api_base_url: String,
    /// The checkpoint gate directory the server forwards to its workers.
    gates_dir:               PathBuf,
    /// Extra environment on the server process, kept for a relaunch.
    env:                     Vec<(String, String)>,
}

impl RunningServer {
    pub(super) async fn start() -> Self {
        Self::start_with("", &[]).await
    }

    /// Start with `settings` appended to the server's settings file (the
    /// workers read the same file through `FABRO_CONFIG`) and `secrets`
    /// in the vault before the first launch, so the server and its workers
    /// see them from the start.
    pub(super) async fn start_with(settings: &str, secrets: &[(&str, &str)]) -> Self {
        Self::start_with_env(settings, secrets, &[]).await
    }

    /// `start_with`, plus `env` on the server process: the test hooks the
    /// server forwards to its workers by name.
    pub(super) async fn start_with_env(
        settings: &str,
        secrets: &[(&str, &str)],
        env: &[(&str, &str)],
    ) -> Self {
        let home_root = tempfile::tempdir_in("/tmp").expect("home tempdir");
        let storage_root = isolated_storage_dir();
        let storage_dir = storage_root.path().join("storage");
        let port = reserve_port();
        let config_path = home_root.path().join("settings.toml");
        std::fs::write(
            &config_path,
            format!("_version = 1\n\n[server.auth]\nmethods = [\"dev-token\"]\n{settings}"),
        )
        .expect("the server settings write");
        if !secrets.is_empty() {
            let mut vault = Vault::load(Storage::new(&storage_dir).secrets_path())
                .expect("the server vault loads");
            for (name, value) in secrets {
                vault
                    .set(name, value, SecretType::Token, None)
                    .expect("the secret stores in the server vault");
            }
        }
        let runtime_directory = Storage::new(&storage_dir).runtime_directory();
        envfile::merge_env_file(&runtime_directory.env_path(), [
            ("SESSION_SECRET", TEST_SESSION_SECRET),
            ("FABRO_DEV_TOKEN", TEST_DEV_TOKEN),
        ])
        .expect("the server env writes");
        fabro_util::dev_token::write_dev_token(&runtime_directory.dev_token_path(), TEST_DEV_TOKEN)
            .expect("the dev token writes");
        let gates_dir = home_root.path().join("checkpoint-gates");
        std::fs::create_dir_all(&gates_dir).expect("the gates dir creates");
        let mut server = Self {
            child: None,
            home_root,
            _storage_root: storage_root,
            storage_dir,
            config_path,
            port,
            api_base_url: format!("http://127.0.0.1:{port}"),
            gates_dir,
            env: env
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        };
        server.launch().await;
        server
    }

    /// Start the server process over this storage; the same call brings
    /// it back after a kill.
    pub(super) async fn launch(&mut self) {
        assert!(self.child.is_none(), "the server is already running");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut cmd, self.home_root.path());
        // The resume scenario restarts the server: its object store must
        // outlive the process.
        cmd.env(EnvVars::FABRO_TEST_IN_MEMORY_STORE, "0");
        cmd.env(
            EnvVars::FABRO_HOME,
            self.home_root.path().join("fabro-home"),
        );
        cmd.env(EnvVars::FABRO_TEST_CHECKPOINT_GATES, &self.gates_dir);
        for (name, value) in &self.env {
            cmd.env(name, value);
        }
        cmd.args(["server", "start", "--foreground"])
            .arg("--storage-dir")
            .arg(&self.storage_dir)
            .arg("--bind")
            .arg(format!("127.0.0.1:{}", self.port))
            .arg("--config")
            .arg(&self.config_path)
            .stdin(Stdio::null())
            .stdout(self.stderr_log())
            .stderr(self.stderr_log());
        let mut child = cmd.spawn().expect("the server spawns");
        let log_path = self.storage_dir.with_file_name("server.stderr.log");
        wait_for_http_ready(&self.api_base_url, &mut child, &log_path).await;
        self.child = Some(child);
    }

    /// Where the server's stdout and stderr go: a file beside its storage,
    /// so a chatty server never blocks on a pipe nobody reads, and a
    /// failing test can show its log.
    fn stderr_log(&self) -> Stdio {
        let path = self.storage_dir.with_file_name("server.stderr.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("the server stderr log opens");
        Stdio::from(file)
    }

    pub(super) fn stderr_text(&self) -> String {
        std::fs::read_to_string(self.storage_dir.with_file_name("server.stderr.log"))
            .unwrap_or_default()
    }

    /// The `--server` target a CLI command reaches this server at.
    pub(super) fn target(&self) -> String {
        format!("{}/api/v1", self.api_base_url)
    }

    /// Kill the server outright, as a crash would; its workers live on in
    /// their own process groups.
    pub(super) fn kill(&mut self) {
        let mut child = self.child.take().expect("the server is running");
        child.kill().expect("the server dies");
        let _ = child.wait();
    }

    pub(super) fn shutdown(mut self) {
        let mut stop = Command::new(env!("CARGO_BIN_EXE_fabro"));
        apply_test_isolation(&mut stop, self.home_root.path());
        stop.args(["server", "stop"])
            .arg("--storage-dir")
            .arg(&self.storage_dir);
        let output = stop.output().expect("server stop runs");
        assert!(
            output.status.success(),
            "server stop failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let status = self
            .child
            .take()
            .expect("the server is running")
            .wait()
            .expect("the server exit status reads");
        assert!(
            status.success(),
            "the server exited unsuccessfully\nstderr:\n{}",
            self.stderr_text()
        );
    }

    /// Petri's store over the server's database, read beside the server:
    /// what `petri inspect` would see.
    pub(super) async fn petri_store(&self) -> SqliteRunStore {
        let database = fabro_db::Database::connect(Storage::new(&self.storage_dir).sqlite_path())
            .await
            .expect("the server database opens");
        SqliteRunStore::new(database.clone_pool())
    }

    /// The run's platform records in the server's database.
    async fn platform_records(&self) -> PlatformRecordStore {
        let database = fabro_db::Database::connect(Storage::new(&self.storage_dir).sqlite_path())
            .await
            .expect("the server database opens");
        PlatformRecordStore::new(database.clone_pool())
    }

    /// Where the run's worker ran Petri: the run's scratch under the
    /// server's storage.
    pub(super) fn petri_run_dir(&self, run_id: &str) -> PathBuf {
        let run_id: RunId = run_id.parse().expect("the run id parses");
        Storage::new(&self.storage_dir)
            .run_scratch(&run_id)
            .root()
            .join("petri")
    }

    /// The worker's own log for the run.
    pub(super) fn worker_log(&self, run_id: &str) -> PathBuf {
        let run_id: RunId = run_id.parse().expect("the run id parses");
        Storage::new(&self.storage_dir)
            .run_scratch(&run_id)
            .root()
            .join("runtime")
            .join("server.log")
    }

    /// Hold the worker's checkpoint at `point` (`commit` or `record`) for
    /// `node` until [`release`](Self::release).
    pub(super) fn hold(&self, point: &str, node: &str) {
        std::fs::write(self.gates_dir.join(format!("{point}.{node}.hold")), "")
            .expect("the hold file writes");
    }

    pub(super) fn release(&self, point: &str, node: &str) {
        std::fs::write(self.gates_dir.join(format!("{point}.{node}.release")), "")
            .expect("the release file writes");
    }

    /// Wait until the worker's log says its checkpoint is held at a gate.
    pub(super) fn wait_until_held(&self, run_id: &str, point: &str, node: &str) {
        let log = self.worker_log(run_id);
        let needle = format!("checkpoint held at a test gate point=\"{point}\" node=\"{node}\"");
        let deadline = Instant::now() + RUN_TIMEOUT;
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains(&needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the worker never held at {point}.{node}; log:\n{text}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// The one host workspace of the run, and the commits on its run
    /// branch, oldest first, as `(subject, key)`.
    fn workspace_commits(&self, run_id: &str) -> (PathBuf, Vec<(String, Option<CheckpointKey>)>) {
        let scopes = self.petri_run_dir(run_id).join("scopes");
        let mut workspaces: Vec<PathBuf> = std::fs::read_dir(&scopes)
            .expect("the scopes directory lists")
            .map(|entry| entry.expect("an entry reads").path().join("work"))
            .collect();
        assert_eq!(workspaces.len(), 1, "one workspace: {workspaces:?}");
        let workspace = workspaces.remove(0);
        let output = Command::new("git")
            .args(["log", "--reverse", "--format=%s%x00%B%x1e"])
            .current_dir(&workspace)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = String::from_utf8_lossy(&output.stdout).into_owned();
        let commits = log
            .split('\u{1e}')
            .filter(|entry| !entry.trim().is_empty())
            .map(|entry| {
                let mut parts = entry.trim_start().splitn(2, '\0');
                let subject = parts.next().unwrap_or_default().to_string();
                let body = parts.next().unwrap_or_default();
                (subject, CheckpointKey::from_message(body))
            })
            .collect();
        (workspace, commits)
    }

    /// The run's checkpoint records, in seq order, as `(node position, sha)`.
    pub(super) async fn checkpoints(&self, run_id: &str) -> Vec<(CheckpointKey, String)> {
        let run_id: RunId = run_id.parse().expect("the run id parses");
        self.platform_records()
            .await
            .read_kind(&run_id, PlatformRecordKind::Checkpoint)
            .await
            .expect("the checkpoint records read")
            .into_iter()
            .filter_map(|stored| match stored.record {
                PlatformRecord::Checkpoint(record) => Some((
                    CheckpointKey::from_operation(record.operation.as_ref()?)?,
                    record.git_commit_sha?,
                )),
                _ => None,
            })
            .collect()
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn reserve_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a port binds")
        .local_addr()
        .expect("the listener has an address")
        .port()
}

async fn wait_for_http_ready(base_url: &str, child: &mut Child, log_path: &Path) {
    let client = fabro_test::test_http_client();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.get(format!("{base_url}/health")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(_) | Err(_) if Instant::now() < deadline => {
                if let Some(status) = child.try_wait().expect("the server polls") {
                    let log = std::fs::read_to_string(log_path).unwrap_or_default();
                    let tail = log.lines().rev().take(20).collect::<Vec<_>>();
                    panic!(
                        "the server exited before it was ready with status {status}; its log ends \
                         with:\n{}",
                        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                    );
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(response) => panic!("server at {base_url} was not ready: {}", response.status()),
            Err(err) => panic!("server at {base_url} was not ready: {err}"),
        }
    }
}

/// A workspace holding a command-only bundle whose `workflow.toml` names
/// Petri, with the given stage script.
fn write_petri_workspace(context: &fabro_test::TestContext, script: &str) -> PathBuf {
    write_petri_workflow(
        context,
        &format!(
            "digraph Command {{\n  graph [goal=\"Run one command\", default_max_retries=0]\n  start \
             [shape=Mdiamond]\n  exit [shape=Msquare]\n  say [shape=parallelogram, \
             script=\"{script}\", max_retries=0]\n  start -> say -> exit\n}}\n"
        ),
    )
}

/// A workspace holding the given workflow with a `workflow.toml` that names
/// Petri.
pub(super) fn write_petri_workflow(context: &fabro_test::TestContext, dot: &str) -> PathBuf {
    let workspace = context.temp_dir.join("petri-workspace");
    std::fs::create_dir_all(&workspace).expect("the workspace creates");
    std::fs::write(workspace.join("workflow.fabro"), dot).expect("the workflow writes");
    std::fs::write(
        workspace.join("workflow.toml"),
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n\n[run]\ngoal = \"Run one \
         command\"\n",
    )
    .expect("the settings write");
    workspace
}

/// `fabro run --detach --auto-approve` against the server: the run is
/// created and started, and its id comes back.
pub(super) fn run_detached(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    workspace: &Path,
) -> String {
    run_detached_with(context, server, workspace, &["--auto-approve"])
}

/// `fabro run --detach` against the server with extra arguments, such as
/// `--auto-approve` or the model to run the workflow's agents on.
pub(super) fn run_detached_with(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    workspace: &Path,
    extra: &[&str],
) -> String {
    run_detached_in(context, server, workspace, "local", extra)
}

/// `fabro run --detach` on the server's environment `environment`.
pub(super) fn run_detached_in(
    context: &fabro_test::TestContext,
    server: &RunningServer,
    workspace: &Path,
    environment: &str,
    extra: &[&str],
) -> String {
    let target = server.target();
    seed_dev_token_auth(
        &context.home_dir,
        &ServerTarget::http_url(&target).expect("the target parses"),
        TEST_DEV_TOKEN,
    );
    let output = context
        .run_cmd()
        .current_dir(workspace)
        .args(["--server", &target, "--detach"])
        .args(extra)
        .args(["--environment", environment, "workflow.toml"])
        .output()
        .expect("the detached run executes");
    assert!(
        output.status.success(),
        "detached run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    created_run_id(&output)
}

pub(super) async fn run_json(server: &RunningServer, path: &str) -> serde_json::Value {
    let response = fabro_test::test_http_client()
        .get(format!("{}/api/v1/{path}", server.api_base_url))
        .bearer_auth(TEST_DEV_TOKEN)
        .send()
        .await
        .expect("the request sends");
    expect_reqwest_json(
        response,
        fabro_http::StatusCode::OK,
        format!("GET /api/v1/{path}"),
    )
    .await
}

pub(super) async fn run_status(server: &RunningServer, run_id: &str) -> String {
    run_json(server, &format!("runs/{run_id}")).await["lifecycle"]["status"]["kind"]
        .as_str()
        .expect("the run has a status kind")
        .to_string()
}

pub(super) async fn wait_for_status(
    server: &RunningServer,
    run_id: &str,
    expected: &[&str],
) -> String {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        let status = run_status(server, run_id).await;
        if expected.contains(&status.as_str()) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not reach {expected:?}; last status {status}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The run's stream, as `GET /runs/{id}/events` serves a Petri run: every
/// item in `stream_seq` order, in the stream envelope.
pub(super) async fn run_stream(server: &RunningServer, run_id: &str) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    let mut after = 0;
    loop {
        let page = run_json(
            server,
            &format!("runs/{run_id}/events?after={after}&limit=1000"),
        )
        .await;
        let data = page["data"]
            .as_array()
            .cloned()
            .expect("the stream page has a data array");
        let Some(last) = data.last() else {
            break;
        };
        after = last["stream_seq"].as_u64().expect("a stream_seq");
        let has_more = page["meta"]["has_more"].as_bool().unwrap_or(false);
        items.extend(data);
        if !has_more {
            break;
        }
    }
    items
}

/// What each stream item is, for an assertion: a Petri event by its
/// `<subject>.<verb>` name (`question` and `question_expired` for the parsed
/// progress payloads), a platform lifecycle record as
/// `lifecycle:<transition>`, another platform record by its kind.
pub(super) fn stream_names(items: &[serde_json::Value]) -> Vec<String> {
    items
        .iter()
        .map(|line| {
            let item = &line["item"];
            if line["kind"] == "platform" {
                let record = &item["record"];
                return match record["kind"].as_str().unwrap_or("?") {
                    "run.lifecycle" => {
                        format!("lifecycle:{}", record["transition"].as_str().unwrap_or("?"))
                    }
                    kind => kind.to_string(),
                };
            }
            if let Some(kind) = item["derived"]["parsed"]["kind"].as_str() {
                if matches!(kind, "question" | "question_expired") {
                    return kind.to_string();
                }
            }
            item["record"]["body"]["event"]
                .as_str()
                .or_else(|| item["derived"]["event"].as_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect()
}

pub(super) fn count_of(names: &[String], expected: &str) -> usize {
    names.iter().filter(|name| *name == expected).count()
}

/// The run's whole stream once it is settled: Fabro's terminal lifecycle
/// record lands a moment after the engine's finish (the worker exits, the
/// server records the status, the projector folds it), so a reader that
/// wants the end of the stream waits for that record.
pub(super) async fn settled_stream(server: &RunningServer, run_id: &str) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        let items = run_stream(server, run_id).await;
        let names = stream_names(&items);
        if names
            .iter()
            .any(|name| matches!(name.as_str(), "lifecycle:succeeded" | "lifecycle:failed"))
        {
            return items;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} never recorded its terminal lifecycle transition: {names:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The pid of the worker subprocess the server launched for the run: the
/// worker retitles itself `fabro <first 12 of the run id> <phase>`, so that
/// is what the process table shows.
fn worker_pid(run_id: &str) -> Option<u32> {
    let short_id: String = run_id.chars().take(12).collect();
    let output = Command::new("pgrep")
        .args(["-f", &format!("^fabro {short_id} ")])
        .output()
        .expect("pgrep runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse().ok())
}

pub(super) fn wait_for_worker(run_id: &str) -> u32 {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(pid) = worker_pid(run_id) {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "no worker process appeared for run {run_id}"
        );
        std::thread::sleep(POLL);
    }
}

/// Whether a process is waiting on the gate file: the stage is mid-flight.
pub(super) fn gate_is_polled(gate: &Path) -> bool {
    let output = Command::new("pgrep")
        .args(["-f", &gate.display().to_string()])
        .output()
        .expect("pgrep runs");
    output.status.success()
}

pub(super) fn wait_until_gate_is_polled(gate: &Path) {
    let deadline = Instant::now() + RUN_TIMEOUT;
    while !gate_is_polled(gate) {
        assert!(
            Instant::now() < deadline,
            "the stage never started waiting on {}",
            gate.display()
        );
        std::thread::sleep(POLL);
    }
}

/// A command-only Petri bundle runs to completion in the worker the server
/// launched: Fabro reports the run succeeded, the worker wrote Petri's
/// records through the HTTP store, and its lease ended with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_petri_run_executes_in_the_server_launched_worker() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let workspace = write_petri_workspace(&context, "echo hello from petri");
    let run_id = run_detached(&context, &server, &workspace);

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&server, &format!("runs/{run_id}")).await;
    assert_eq!(status, "succeeded", "run: {run}");
    let state = run_json(&server, &format!("runs/{run_id}/state")).await;
    assert!(
        state["spec"]["admission"]["graph"]["digest"].is_string(),
        "state: {state}"
    );

    let names = stream_names(&settled_stream(&server, &run_id).await);
    assert_eq!(count_of(&names, "lifecycle:succeeded"), 1, "{names:?}");
    assert_eq!(count_of(&names, "run.finished"), 1, "{names:?}");
    assert!(
        names.iter().any(|name| name == "lifecycle:starting")
            && names.iter().any(|name| name == "lifecycle:running"),
        "{names:?}"
    );

    let store = server.petri_store().await;
    let outcome = engine::outcome_of(&store, &run_id)
        .await
        .expect("the run's Petri record inspects");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    let key = RunKey::new(run_id.as_str());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let holder = store.owner(&key).await.expect("the lease reads");
        if holder.is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker's lease {holder:?} outlived the worker"
        );
        tokio::time::sleep(POLL).await;
    }
    server.shutdown();
}

/// A Petri run whose worker and server both die mid-stage continues after
/// the server restarts: the new server releases the dead worker's lease,
/// asks the run to start again as a resume, and launches a worker in
/// resume mode, which finishes the run with one terminal lifecycle record.
#[tokio::test(flavor = "multi_thread")]
async fn a_petri_run_resumes_in_a_new_worker_after_the_server_restarts() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("resume.gate");
    let script = format!("while [ ! -f {} ]; do sleep 0.05; done", gate.display());
    let workspace = write_petri_workspace(&context, &script);
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    eprintln!("run {run_id} is running");
    let worker = wait_for_worker(&run_id);
    eprintln!("worker {worker} launched");
    wait_until_gate_is_polled(&gate);
    eprintln!("stage is waiting on the gate");

    // The crash: the server first, so it never observes the worker exit,
    // then the worker's whole process group, plugin and stage included.
    server.kill();
    fabro_proc::sigkill_process_group(worker);
    let deadline = Instant::now() + Duration::from_secs(10);
    while fabro_proc::process_running(worker) {
        assert!(Instant::now() < deadline, "the worker did not die");
        std::thread::sleep(POLL);
    }
    assert_eq!(
        run_status_offline(&server).await,
        None,
        "the server is down"
    );

    server.launch().await;
    eprintln!("server restarted");
    let status = wait_for_status(&server, &run_id, &["running", "succeeded", "failed"]).await;
    eprintln!("run {run_id} is {status} after the restart");
    let resumed = wait_for_worker(&run_id);
    assert_ne!(resumed, worker, "a new worker was launched");
    eprintln!("worker {resumed} launched for the resume");
    wait_until_gate_is_polled(&gate);
    eprintln!("stage is waiting on the gate again");
    std::fs::write(&gate, "go").expect("the gate opens");

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let items = settled_stream(&server, &run_id).await;
    let names = stream_names(&items);
    assert_eq!(
        status,
        "succeeded",
        "stream: {names:?}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert_eq!(count_of(&names, "lifecycle:succeeded"), 1, "{names:?}");
    assert_eq!(count_of(&names, "run.finished"), 1, "{names:?}");
    // `fabro run` asked for the first start; the restart asked for a
    // resume, after the run had been running.
    let first_running = names
        .iter()
        .position(|name| name == "lifecycle:running")
        .expect("the run ran before the crash");
    let resume_request = items
        .iter()
        .position(|line| {
            let record = &line["item"]["record"];
            line["kind"] == "platform"
                && record["kind"] == "run.lifecycle"
                && record["transition"] == "start_requested"
                && record["source"] == "resume"
        })
        .expect("the restart asked for a resume");
    assert!(resume_request > first_running, "{names:?}");
    assert_eq!(
        count_of(&names, "lifecycle:running"),
        2,
        "the run ran once before and once after the restart: {names:?}"
    );

    let store = server.petri_store().await;
    let outcome = engine::outcome_of(&store, &run_id)
        .await
        .expect("the run's Petri record inspects");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    server.shutdown();
}

/// The run's status while the server may be down: `None` when it is.
async fn run_status_offline(server: &RunningServer) -> Option<String> {
    fabro_test::test_http_client()
        .get(format!("{}/health", server.api_base_url))
        .send()
        .await
        .ok()
        .map(|response| response.status().to_string())
}

/// The run's pending questions, as the API lists them.
pub(super) async fn questions(server: &RunningServer, run_id: &str) -> Vec<serde_json::Value> {
    run_json(server, &format!("runs/{run_id}/questions")).await["data"]
        .as_array()
        .cloned()
        .expect("the questions list is an array")
}

/// Wait until `count` questions are pending at once.
pub(super) async fn wait_for_questions(
    server: &RunningServer,
    run_id: &str,
    count: usize,
) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        let pending = questions(server, run_id).await;
        if pending.len() >= count {
            return pending;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not ask {count} question(s); pending: {pending:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Answer a question through the API, as the web app and the CLI do. The
/// question id is Petri's (`gate#2`), so it travels as one percent-encoded
/// path segment, as the generated clients send it.
pub(super) async fn answer(
    server: &RunningServer,
    run_id: &str,
    question_id: &str,
    body: serde_json::Value,
) {
    let mut url = fabro_http::Url::parse(&format!(
        "{}/api/v1/runs/{run_id}/questions",
        server.api_base_url
    ))
    .expect("the API base URL parses");
    url.path_segments_mut()
        .expect("the API URL has a path")
        .push(question_id)
        .push("answer");
    let response = fabro_test::test_http_client()
        .post(url)
        .bearer_auth(TEST_DEV_TOKEN)
        .json(&body)
        .send()
        .await
        .expect("the answer sends");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(
        status,
        fabro_http::StatusCode::NO_CONTENT,
        "POST /api/v1/runs/{run_id}/questions/{question_id}/answer: {body}"
    );
}

/// A yes/no gate whose branches each leave a marker file.
fn gate_dot(markers: &Path, gate_attrs: &str) -> String {
    format!(
        "digraph Gate {{\n  graph [goal=\"Ask before running\"]\n  start [shape=Mdiamond]\n  \
         exit [shape=Msquare]\n  gate [shape=hexagon, label=\"Go?\", \
         question_type=\"yes_no\"{gate_attrs}]\n  yes [shape=parallelogram, script=\"touch \
         {dir}/yes\"]\n  no [shape=parallelogram, script=\"touch {dir}/no\"]\n  start -> gate\n  \
         gate -> yes [label=\"[Y] Yes\"]\n  gate -> no [label=\"[N] No\"]\n  yes -> exit\n  no \
         -> exit\n}}\n",
        dir = markers.display()
    )
}

/// Two gates as the branches of one parallel node; the join's results are
/// written out, so each gate's answer is read from its branch result.
fn two_gates_dot(markers: &Path) -> String {
    format!(
        "digraph Gates {{\n  graph [goal=\"Ask twice at once\"]\n  start [shape=Mdiamond]\n  \
         exit [shape=Msquare]\n  fan [shape=component]\n  a [shape=hexagon, label=\"A?\", \
         question_type=\"yes_no\"]\n  b [shape=hexagon, label=\"B?\", \
         question_type=\"yes_no\"]\n  join [shape=tripleoctagon]\n  report \
         [shape=parallelogram, script=\"cat > {dir}/results.json\", \
         stdin_source=\"context.parallel.results\"]\n  start -> fan\n  fan -> a\n  fan -> b\n  \
         a -> join [label=\"[Y] Yes\"]\n  a -> join [label=\"[N] No\"]\n  b -> join [label=\"[Y] \
         Yes\"]\n  b -> join [label=\"[N] No\"]\n  join -> report -> exit\n}}\n",
        dir = markers.display()
    )
}

/// A human gate in the worker asks through the server: the question is
/// listed by the questions API with the gate's stage and options, the
/// answer reaches the worker over its control channel and routes the gate,
/// and the run's stream records the interview.
#[tokio::test(flavor = "multi_thread")]
async fn a_human_gate_in_the_worker_is_answered_through_the_api() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let markers = context.temp_dir.join("markers");
    std::fs::create_dir_all(&markers).expect("the marker dir creates");
    let workspace = write_petri_workflow(&context, &gate_dot(&markers, ""));
    let run_id = run_detached_with(&context, &server, &workspace, &[]);

    let pending = wait_for_questions(&server, &run_id, 1).await;
    let question = &pending[0];
    assert_eq!(question["stage"], "gate@1", "{question}");
    assert_eq!(question["question_type"], "yes_no", "{question}");
    let question_id = question["id"].as_str().expect("an id").to_string();
    assert!(
        question_id.starts_with("gate#"),
        "Petri's id: {question_id}"
    );
    answer(
        &server,
        &run_id,
        &question_id,
        serde_json::json!({ "kind": "no" }),
    )
    .await;

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "server stderr:\n{}",
        server.stderr_text()
    );
    assert!(
        markers.join("no").exists() && !markers.join("yes").exists(),
        "the no branch ran"
    );
    let names = stream_names(&run_stream(&server, &run_id).await);
    assert!(
        names.iter().any(|name| name == "question")
            && names.iter().any(|name| name == "interview.answered"),
        "the question and who answered it are on the stream: {names:?}"
    );
    assert!(questions(&server, &run_id).await.is_empty());
    let store = server.petri_store().await;
    let outcome = engine::outcome_of(&store, &run_id)
        .await
        .expect("the run's Petri record inspects");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    server.shutdown();
}

/// Two branches of a parallel node ask at once; each answer, given through
/// the API in the other order, binds to its own branch.
#[tokio::test(flavor = "multi_thread")]
async fn two_parallel_gates_in_the_worker_each_bind_their_own_answer() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let markers = context.temp_dir.join("markers");
    std::fs::create_dir_all(&markers).expect("the marker dir creates");
    let workspace = write_petri_workflow(&context, &two_gates_dot(&markers));
    let run_id = run_detached_with(&context, &server, &workspace, &[]);

    let pending = wait_for_questions(&server, &run_id, 2).await;
    let id_of = |stage: &str| {
        pending
            .iter()
            .find(|question| question["stage"] == stage)
            .and_then(|question| question["id"].as_str())
            .unwrap_or_else(|| panic!("`{stage}` is pending: {pending:?}"))
            .to_string()
    };
    let (a, b) = (id_of("a@1"), id_of("b@1"));
    assert_ne!(a, b);
    for question in &pending {
        assert_eq!(question["question_type"], "yes_no", "{question}");
    }
    // A yes/no question takes `yes` or `no`, as the API validates it.
    answer(&server, &run_id, &b, serde_json::json!({ "kind": "yes" })).await;
    answer(&server, &run_id, &a, serde_json::json!({ "kind": "no" })).await;

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "server stderr:\n{}",
        server.stderr_text()
    );
    let results: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(markers.join("results.json")).expect("the join wrote its results"),
    )
    .expect("the results parse");
    let results = results.as_array().expect("a list of branch results");
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0]["id"], "a");
    assert_eq!(results[0]["context_updates"]["human.gate.selected"], "N");
    assert_eq!(results[1]["id"], "b");
    assert_eq!(results[1]["context_updates"]["human.gate.selected"], "Y");
    server.shutdown();
}

/// A gate nobody answers expires on its own deadline: the run takes the
/// gate's default, the stream records the timeout, and nothing stays
/// pending.
#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_gate_in_the_worker_expires_with_its_default() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let markers = context.temp_dir.join("markers");
    std::fs::create_dir_all(&markers).expect("the marker dir creates");
    let workspace = write_petri_workflow(
        &context,
        &gate_dot(&markers, ", timeout=\"2s\", human.default_choice=\"no\""),
    );
    let run_id = run_detached_with(&context, &server, &workspace, &[]);

    let pending = wait_for_questions(&server, &run_id, 1).await;
    assert_eq!(pending[0]["timeout_seconds"], 2.0, "{}", pending[0]);

    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "server stderr:\n{}",
        server.stderr_text()
    );
    assert!(
        markers.join("no").exists() && !markers.join("yes").exists(),
        "the default ran"
    );
    let names = stream_names(&run_stream(&server, &run_id).await);
    assert!(
        names.iter().any(|name| name == "question_expired"),
        "{names:?}"
    );
    assert!(questions(&server, &run_id).await.is_empty());
    server.shutdown();
}

/// A CLI command against the server, as `run_detached` seeds its auth.
fn cli(context: &fabro_test::TestContext, server: &RunningServer, args: &[&str]) -> Output {
    let target = server.target();
    let output = context
        .command()
        .args(args)
        .args(["--server", &target])
        .output()
        .expect("the CLI command executes");
    assert!(
        output.status.success(),
        "`fabro {}` failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn ndjson(output: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

/// The `<subject>.<verb>` name of a Petri item, or the kind of a platform
/// record, from a raw stream line.
fn stream_line_name(line: &serde_json::Value) -> String {
    let item = &line["item"];
    if line["kind"] == "platform" {
        return format!(
            "platform:{}",
            item["record"]["kind"].as_str().unwrap_or("?")
        );
    }
    item["record"]["body"]["event"]
        .as_str()
        .or_else(|| item["derived"]["event"].as_str())
        .unwrap_or("?")
        .to_string()
}

/// The snapshot filters for `events --pretty` over a Petri run: clocks,
/// durations and the run id vary per run.
fn pretty_filters(context: &fabro_test::TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b\d{2}:\d{2}:\d{2}\b".to_string(), "[CLOCK]".to_string()));
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    filters.push((
        r"Checkpoint [0-9a-f]{7}".to_string(),
        "Checkpoint [SHA]".to_string(),
    ));
    filters
}

/// A finished Petri run reads back through the CLI: `events` prints the
/// stream envelope raw, dense in `stream_seq`; `events --pretty` renders
/// the stages by `<subject>.<verb>` with their labels and the platform
/// records by kind; `attach` replays it and exits with the run's status;
/// `wait` and `runs inspect` read the projection.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_petri_run_reads_back_through_the_cli() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let workspace = write_petri_workspace(&context, "echo hello from petri");
    let run_id = run_detached(&context, &server, &workspace);
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "server stderr:\n{}",
        server.stderr_text()
    );
    settled_stream(&server, &run_id).await;
    let target = server.target();

    // Raw: the envelope, one item per line, dense and in order.
    let raw = cli(&context, &server, &["events", &run_id]);
    let lines = ndjson(&raw);
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["stream_seq"].as_u64().expect("a stream_seq"))
        .collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "stream_seq is dense");
    for line in &lines {
        assert_eq!(line["run_id"], run_id, "{line}");
        assert!(
            line["id"].is_string() && line["recorded_at"].is_u64(),
            "{line}"
        );
    }
    let names: Vec<String> = lines.iter().map(stream_line_name).collect();
    for expected in [
        "platform:run.created",
        "platform:run.lifecycle",
        "run.started",
        "visit.started",
        "visit.completed",
        "run.finished",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} is on the stream: {names:?}"
        );
    }
    assert_eq!(
        names.last().map(String::as_str),
        Some("platform:run.lifecycle"),
        "the terminal lifecycle record ends the stream: {names:?}"
    );

    // Tail: the last two items only.
    let tail = cli(&context, &server, &["events", "--tail", "2", &run_id]);
    assert_eq!(ndjson(&tail).len(), 2);

    // Pretty: stages and platform records.
    let mut cmd = context.command();
    cmd.args(["events", "--pretty", "--server", &target, &run_id]);
    fabro_snapshot!(pretty_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [CLOCK] ▶ Run one command  [ULID]
    [CLOCK]   · submitted
    [CLOCK]   · start_requested
    [CLOCK]   · runnable
    [CLOCK]   · starting
    [CLOCK]   · running
    [CLOCK]   Engine: petri run started
    [CLOCK] ▶ start
    [CLOCK]    │ checkout: [TEMP_DIR]/petri-workspace is not a Git repository; the workspace starts empty
    [CLOCK] ✓ start  [DURATION]
    [CLOCK]   Branch: fabro/run/[ULID] from [SHA]
    [CLOCK]   Git identity: Fabro <noreply@fabro.sh>  default
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK] ▶ say
    [CLOCK]    start → say continue
    [CLOCK]    │ hello from petri
    [CLOCK] ✓ say  [DURATION]
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK] ▶ exit
    [CLOCK]    say → exit continue
    [CLOCK] ✓ exit  [DURATION]
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK]   Diff: +0 -0 in 0 file(s)
    [CLOCK] ✓ SUCCEEDED [DURATION]
    [CLOCK]   · succeeded
    ----- stderr -----
    ");

    // Attach replays the finished run and exits with its status.
    let attach = cli(&context, &server, &["attach", &run_id]);
    let stderr = String::from_utf8_lossy(&attach.stderr);
    assert!(stderr.contains("say"), "the stage is drawn: {stderr}");

    // Wait reads the projection's status and conclusion.
    let wait = cli(&context, &server, &["wait", &run_id]);
    let stderr = String::from_utf8_lossy(&wait.stderr);
    assert!(stderr.contains("Succeeded"), "{stderr}");

    // Inspect reads the projection, whose spec names the admission.
    let inspect = cli(&context, &server, &["inspect", &run_id]);
    let inspected: serde_json::Value =
        serde_json::from_slice(&inspect.stdout).expect("inspect prints JSON");
    let entry = &inspected[0];
    assert_eq!(entry["run_id"], run_id, "{entry}");
    assert!(
        entry["run_spec"]["admission"]["graph"]["digest"].is_string(),
        "{entry}"
    );
    assert_eq!(entry["conclusion"]["status"], "succeeded", "{entry}");
    server.shutdown();
}

/// `attach` on a Petri run with a human gate asks the question at the
/// terminal and answers it through the questions API; the answer routes
/// the gate and the attach exits with the run's status.
#[tokio::test(flavor = "multi_thread")]
async fn attach_asks_a_petri_gate_at_the_terminal_and_answers_it() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let markers = context.temp_dir.join("markers");
    std::fs::create_dir_all(&markers).expect("the marker dir creates");
    let workspace = write_petri_workflow(&context, &gate_dot(&markers, ""));
    let run_id = run_detached_with(&context, &server, &workspace, &[]);
    wait_for_questions(&server, &run_id, 1).await;

    let target = server.target();
    let mut attach_cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut attach_cmd, &context.home_dir);
    attach_cmd
        .current_dir(&context.temp_dir)
        .args(["attach", "--server", &target, &run_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = attach_cmd.spawn().expect("attach spawns");
    {
        let mut stdin = child.stdin.take().expect("attach stdin is piped");
        stdin.write_all(b"N\n").expect("the answer writes");
    }
    let output = child
        .wait_with_output()
        .expect("attach exits once the run ends");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "attach failed\nstderr:\n{stderr}\nserver stderr:\n{}",
        server.stderr_text()
    );
    assert!(stderr.contains("Go?"), "the question was asked: {stderr}");
    assert!(
        markers.join("no").exists() && !markers.join("yes").exists(),
        "the no branch ran"
    );
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    assert_eq!(status, "succeeded");
    server.shutdown();
}

/// `events --follow` on a Petri run follows the stream live from its
/// cursor: the items already stored print first, the ones committed while
/// the run goes on follow, and the terminal lifecycle record ends it.
#[tokio::test(flavor = "multi_thread")]
async fn events_follow_streams_a_petri_run_live_to_its_end() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let server = RunningServer::start().await;
    let gate = context.temp_dir.join("go");
    let workspace = write_petri_workspace(
        &context,
        &format!(
            "while [ ! -f {} ]; do sleep 0.05; done; echo released",
            gate.display()
        ),
    );
    let run_id = run_detached(&context, &server, &workspace);
    wait_for_status(&server, &run_id, &["running"]).await;

    let target = server.target();
    let mut follow_cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
    apply_test_isolation(&mut follow_cmd, &context.home_dir);
    follow_cmd
        .current_dir(&context.temp_dir)
        .args([
            "events", "--follow", "--pretty", "--server", &target, &run_id,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = follow_cmd.spawn().expect("events --follow spawns");
    // Let the follower attach before the run is released.
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::fs::write(&gate, b"").expect("the release marker writes");

    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("the follower polls") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "events --follow did not end with the run; server stderr:\n{}",
            server.stderr_text()
        );
        tokio::time::sleep(POLL).await;
    };
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("stdout reads");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr reads");
    assert!(status.success(), "events --follow failed: {stderr}");
    assert!(stdout.contains("▶ say"), "the stage started: {stdout}");
    assert!(stdout.contains("│ released"), "the live log line: {stdout}");
    assert!(stdout.contains("✓ SUCCEEDED"), "the finish: {stdout}");
    assert!(
        stdout.trim_end().ends_with("· succeeded"),
        "the terminal lifecycle record ends the follow: {stdout}"
    );
    server.shutdown();
}

/// A shell loop that waits for `gate` to exist.
fn wait_for(gate: &Path) -> String {
    format!("while [ ! -f {} ]; do sleep 0.05; done", gate.display())
}

/// Three command stages: `one` writes a file, `two` writes another after
/// the gate opens (and logs each run), `three` checks both files.
fn three_stage_bundle(context: &fabro_test::TestContext, gate: &Path) -> PathBuf {
    write_petri_workflow(
        context,
        &format!(
            "digraph Stages {{\n  graph [goal=\"Three stages\", default_max_retries=0]\n  start \
             [shape=Mdiamond]\n  exit [shape=Msquare]\n  one [shape=parallelogram, script=\"echo \
             one > one.txt\"]\n  two [shape=parallelogram, script=\"echo run >> two.log; {}; \
             echo two > two.txt\"]\n  three [shape=parallelogram, script=\"test \\\"$(cat \
             one.txt)\\\" = one && test \\\"$(cat two.txt)\\\" = two && cp two.log \
             three.log\"]\n  start -> one -> two -> three -> exit\n}}\n",
            wait_for(gate)
        ),
    )
}

/// Kill the server first, so it never observes the worker exit, then the
/// worker's whole process group, then the stage's own process group when a
/// stage was waiting on `gate`: a stage process runs in a group of its own
/// under the host plugin, and a machine crash takes it with everything
/// else, where a killed worker alone would leave it writing into the
/// workspace.
pub(super) fn crash(server: &mut RunningServer, worker: u32, gate: Option<&Path>) {
    server.kill();
    fabro_proc::sigkill_process_group(worker);
    let deadline = Instant::now() + Duration::from_secs(10);
    while fabro_proc::process_running(worker) {
        assert!(Instant::now() < deadline, "the worker did not die");
        std::thread::sleep(POLL);
    }
    let Some(gate) = gate else {
        return;
    };
    let output = Command::new("pgrep")
        .args(["-f", &gate.display().to_string()])
        .output()
        .expect("pgrep runs");
    for pid in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
    {
        fabro_proc::sigkill_process_group(pid);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate_is_polled(gate) {
        assert!(Instant::now() < deadline, "the stage did not die");
        std::thread::sleep(POLL);
    }
}

/// Wait for the run to succeed after a restart, with the server's stderr
/// on failure.
pub(super) async fn wait_for_success(server: &RunningServer, run_id: &str) {
    let status = wait_for_status(server, run_id, &["succeeded", "failed"]).await;
    assert_eq!(
        status,
        "succeeded",
        "run: {}\nserver stderr:\n{}",
        run_json(server, &format!("runs/{run_id}")).await,
        server.stderr_text()
    );
}

/// The subjects of the commits on the run branch.
fn subjects(commits: &[(String, Option<CheckpointKey>)]) -> Vec<&str> {
    commits
        .iter()
        .map(|(subject, _)| subject.as_str())
        .collect()
}

/// The commit subjects one run of the three-stage bundle produces.
fn three_stage_subjects(run_id: &str) -> Vec<String> {
    ["start", "one", "two", "three", "exit"]
        .iter()
        .map(|node| format!("fabro({run_id}): {node} (success)"))
        .collect()
}

/// A worker killed after a stage's finish is durable: on the restart the
/// stage's commit is not repeated, the stage in flight reruns on the
/// snapshot (its partial output gone), and the next stage sees both.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_a_durable_finish_keeps_its_one_commit() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("two.gate");
    let workspace = three_stage_bundle(&context, &gate);
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    wait_until_gate_is_polled(&gate);
    crash(&mut server, worker, Some(&gate));

    server.launch().await;
    let resumed = wait_for_worker(&run_id);
    assert_ne!(resumed, worker);
    wait_until_gate_is_polled(&gate);
    std::fs::write(&gate, "go").expect("the gate opens");
    wait_for_success(&server, &run_id).await;

    let (path, commits) = server.workspace_commits(&run_id);
    assert_eq!(subjects(&commits), three_stage_subjects(&run_id));
    assert_eq!(
        std::fs::read_to_string(path.join("three.log")).expect("three copied the log"),
        "run\n",
        "the crashed attempt's partial output was reset before the rerun; server log:\n{}",
        server.stderr_text()
    );
    let checkpoints = server.checkpoints(&run_id).await;
    assert_eq!(checkpoints.len(), 5, "{checkpoints:?}");
    let keys: Vec<Option<CheckpointKey>> = checkpoints.iter().map(|(key, _)| Some(*key)).collect();
    let committed: Vec<Option<CheckpointKey>> = commits.iter().map(|(_, key)| *key).collect();
    assert_eq!(keys, committed);
    server.shutdown();
}

/// A worker killed in `prepare_result` before the commit lands: the finish
/// is not durable, the stage reruns once, and one commit exists for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_the_commit_lands_reruns_the_stage_once() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("two.gate");
    std::fs::write(&gate, "open").expect("the script gate is open from the start");
    let workspace = three_stage_bundle(&context, &gate);
    server.hold("commit", "two");
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    server.wait_until_held(&run_id, "commit", "two");
    crash(&mut server, worker, None);

    server.release("commit", "two");
    server.launch().await;
    wait_for_success(&server, &run_id).await;

    let (path, commits) = server.workspace_commits(&run_id);
    assert_eq!(subjects(&commits), three_stage_subjects(&run_id));
    assert_eq!(
        std::fs::read_to_string(path.join("three.log")).expect("three copied the log"),
        "run\n",
        "the stage reran once, on the snapshot before it"
    );
    let store = server.petri_store().await;
    let outcome = engine::outcome_of(&store, &run_id)
        .await
        .expect("the run's Petri record inspects");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    server.shutdown();
}

/// A worker killed after the commit and its durable finish but before the
/// platform record: the restart reconciles the record from the snapshot
/// repository, the stage does not rerun, and one commit exists for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_the_record_reconciles_it_from_the_run_branch() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("two.gate");
    std::fs::write(&gate, "open").expect("the script gate is open from the start");
    let workspace = three_stage_bundle(&context, &gate);
    server.hold("record", "two");
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    server.wait_until_held(&run_id, "record", "two");
    let before = server.checkpoints(&run_id).await;
    assert_eq!(before.len(), 2, "start and one are recorded: {before:?}");
    crash(&mut server, worker, None);

    server.release("record", "two");
    server.launch().await;
    wait_for_success(&server, &run_id).await;

    let (path, commits) = server.workspace_commits(&run_id);
    assert_eq!(subjects(&commits), three_stage_subjects(&run_id));
    assert_eq!(
        std::fs::read_to_string(path.join("three.log")).expect("three copied the log"),
        "run\n",
        "the stage with a durable finish did not rerun"
    );
    let checkpoints = server.checkpoints(&run_id).await;
    assert_eq!(checkpoints.len(), 5, "{checkpoints:?}");
    let keys: Vec<Option<CheckpointKey>> = checkpoints.iter().map(|(key, _)| Some(*key)).collect();
    let committed: Vec<Option<CheckpointKey>> = commits.iter().map(|(_, key)| *key).collect();
    assert_eq!(
        keys, committed,
        "the reconciled record names the one commit"
    );
    server.shutdown();
}

/// A workspace deleted while the run is down is restored from the snapshot
/// repository, and the next stage sees the checkpoint's files.
#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_workspace_is_restored_from_its_snapshot() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("two.gate");
    let workspace = three_stage_bundle(&context, &gate);
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    wait_until_gate_is_polled(&gate);
    crash(&mut server, worker, Some(&gate));
    let (path, commits) = server.workspace_commits(&run_id);
    assert_eq!(subjects(&commits), three_stage_subjects(&run_id)[..2]);
    std::fs::remove_dir_all(&path).expect("the workspace is deleted");

    server.launch().await;
    wait_until_gate_is_polled(&gate);
    std::fs::write(&gate, "go").expect("the gate opens");
    wait_for_success(&server, &run_id).await;

    let (restored, commits) = server.workspace_commits(&run_id);
    assert_eq!(restored, path);
    assert_eq!(subjects(&commits), three_stage_subjects(&run_id));
    assert_eq!(
        std::fs::read_to_string(restored.join("one.txt")).expect("one.txt was restored"),
        "one\n"
    );
    server.shutdown();
}

/// A stage that fails on its own terms routes to its failure edge on the
/// committed files, and after a crash once the failure is durable the
/// route reruns on the same files.
#[tokio::test(flavor = "multi_thread")]
async fn a_failure_route_sees_the_same_committed_files_after_a_crash() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let gate = context.temp_dir.join("fix.gate");
    let workspace = write_petri_workflow(
        &context,
        &format!(
            "digraph Failure {{\n  graph [goal=\"Route on failure\", default_max_retries=0]\n  \
             start [shape=Mdiamond]\n  exit [shape=Msquare]\n  work [shape=parallelogram, \
             script=\"echo partial > out.txt; exit 1\"]\n  fix [shape=parallelogram, script=\"test \
             \\\"$(cat out.txt)\\\" = partial && echo run >> fix.log && {} && echo fixed >> \
             out.txt\"]\n  start -> work -> exit\n  work -> fix [condition=\"outcome=failed\"]\n  \
             fix -> exit\n}}\n",
            wait_for(&gate)
        ),
    );
    let run_id = run_detached(&context, &server, &workspace);

    wait_for_status(&server, &run_id, &["running"]).await;
    let worker = wait_for_worker(&run_id);
    wait_until_gate_is_polled(&gate);
    // The route is running on the committed failure: the crash lands here.
    crash(&mut server, worker, Some(&gate));

    server.launch().await;
    wait_until_gate_is_polled(&gate);
    std::fs::write(&gate, "go").expect("the gate opens");
    wait_for_success(&server, &run_id).await;

    let (path, commits) = server.workspace_commits(&run_id);
    assert_eq!(subjects(&commits), vec![
        format!("fabro({run_id}): start (success)"),
        format!("fabro({run_id}): work (failure)"),
        format!("fabro({run_id}): fix (success)"),
        format!("fabro({run_id}): exit (success)"),
    ]);
    assert_eq!(
        std::fs::read_to_string(path.join("out.txt")).expect("out.txt"),
        "partial\nfixed\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("fix.log")).expect("fix.log"),
        "run\n",
        "the route saw the failed stage's files, not its own interrupted attempt's"
    );
    server.shutdown();
}

/// A checkpoint commit that fails ends the run: `checkpoint_failed` is
/// recorded, no route runs, the run is reported failed, and a restart
/// leaves it failed without launching a worker.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_checkpoint_fails_the_run_and_a_restart_leaves_it_failed() {
    if host_plugin().is_none() {
        return;
    }
    let context = test_context!();
    let mut server = RunningServer::start().await;
    let workspace = write_petri_workflow(
        &context,
        "digraph Wreck {\n  graph [goal=\"Wreck the repository\", default_max_retries=0]\n  start \
         [shape=Mdiamond]\n  exit [shape=Msquare]\n  wreck [shape=parallelogram, script=\"rm -rf \
         .git && echo garbage > .git\"]\n  next [shape=parallelogram, script=\"echo next > \
         next.txt\"]\n  fix [shape=parallelogram, script=\"echo fix > fix.txt\"]\n  start -> wreck \
         -> next -> exit\n  wreck -> fix [condition=\"outcome=failed\"]\n  fix -> exit\n}\n",
    );
    let run_id = run_detached(&context, &server, &workspace);
    let status = wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&server, &format!("runs/{run_id}")).await;
    assert_eq!(status, "failed", "run: {run}");
    // The run's failure travels on the stream as the platform record of
    // its terminal lifecycle transition, with the failure's message as the
    // reason.
    let failures: Vec<String> = settled_stream(&server, &run_id)
        .await
        .iter()
        .filter_map(|line| {
            let record = &line["item"]["record"];
            (line["kind"] == "platform"
                && record["kind"] == "run.lifecycle"
                && record["transition"] == "failed")
                .then(|| record["reason"].as_str().unwrap_or_default().to_string())
        })
        .collect();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("checkpoint commit of `wreck` failed"),
        "{failures:?}"
    );
    let scopes = server.petri_run_dir(&run_id).join("scopes");
    let work = std::fs::read_dir(&scopes)
        .expect("the scopes directory lists")
        .map(|entry| entry.expect("an entry reads").path().join("work"))
        .next()
        .expect("one workspace");
    assert!(!work.join("next.txt").exists(), "no route ran");
    assert!(!work.join("fix.txt").exists(), "no route ran");

    let store = server.petri_store().await;
    let outcome = engine::outcome_of(&store, &run_id)
        .await
        .expect("the run's Petri record inspects");
    assert_ne!(outcome.status, RunStatus::Success, "{outcome:?}");
    let checkpoints = server.checkpoints(&run_id).await;
    assert_eq!(
        checkpoints.len(),
        1,
        "only start was recorded: {checkpoints:?}"
    );

    // The restart finds the run terminal and launches nothing for it.
    server.kill();
    server.launch().await;
    assert_eq!(run_status(&server, &run_id).await, "failed");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        worker_pid(&run_id),
        None,
        "no worker was launched for the failed run"
    );
    server.shutdown();
}
