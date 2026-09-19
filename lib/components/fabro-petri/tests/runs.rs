//! A Fabro workflow runs through Petri from this crate: the `hello` bundle in
//! memory on the stub registry, and a command-only workflow on the host
//! sandbox through the real step registry.
//!
//! Every run, stubbed or real, acquires its scope's environment through the
//! sandbox-driver host plugin, so both tests skip when that executable is not
//! found, unless `FABRO_REQUIRE_SANDBOX_PLUGINS` is set. Fabro's CI installs
//! the plugin on `PATH` in the sandbox-plugins job and requires it there.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests locate the plugin executable through the process environment"
)]
#![expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use petri_execution::host::{self, HostRun};
use petri_execution::inspect::{self, RunInspection};
use petri_frontend_fabro::Fabro;
use petri_runtime::executor::Retention;
use petri_runtime::frontend::CompileInputs;
use petri_runtime::ir::RunStatus;
use petri_runtime::{RunOptions, Runtime};
use petri_store::{Access, MemoryRunStore, RunKey, RunStore as _};
use tokio::fs;

const HOST_PLUGIN: &str = "sandbox-driver-host";
const HOST_PLUGIN_OVERRIDE: &str = "PETRI_SANDBOX_HOST_PLUGIN";
const REQUIRE_ENV: &str = "FABRO_REQUIRE_SANDBOX_PLUGINS";

/// A command-only workflow: one script stage between start and exit.
const COMMAND_WORKFLOW: &str = r#"digraph Command {
    graph [goal="Run one command"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="echo hello from petri"]
    start -> say -> exit
}"#;

const COMMAND_SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

/// The `.fabro/workflows/hello` bundle checked into this repository.
fn hello_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.fabro/workflows/hello")
}

/// The host plugin as Petri's lookup finds it: the override variable, else
/// the executable on `PATH`. `None`, after saying so, when the test should
/// skip; a panic when the environment forbids a skip.
fn host_plugin() -> Option<PathBuf> {
    let found = env::var_os(HOST_PLUGIN_OVERRIDE)
        .map(PathBuf::from)
        .or_else(|| {
            env::split_paths(&env::var_os("PATH")?)
                .map(|dir| dir.join(HOST_PLUGIN))
                .find(|candidate| candidate.is_file())
        });
    if found.is_none() {
        assert!(
            env::var_os(REQUIRE_ENV).is_none(),
            "{REQUIRE_ENV} is set, but {HOST_PLUGIN} is not on PATH and {HOST_PLUGIN_OVERRIDE} is unset"
        );
        eprintln!("skipping: {HOST_PLUGIN} is not on PATH and {HOST_PLUGIN_OVERRIDE} is unset");
    }
    found
}

/// Write a bundle's files into `<root>/.fabro/workflows/<name>` so the
/// frontend sees a bundle root of its own, with no project settings layer
/// above it. Returns the workflow file.
async fn install_bundle(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let bundle = root.join(".fabro").join("workflows").join(name);
    fs::create_dir_all(&bundle)
        .await
        .expect("the bundle directory is creatable");
    for (file, text) in files {
        fs::write(bundle.join(file), text)
            .await
            .expect("the bundle file is writable");
    }
    bundle.join("workflow.fabro")
}

fn run_options(run_dir: &Path, key: &str) -> RunOptions {
    let mut options = RunOptions::new(run_dir);
    options.grace = Duration::from_secs(2);
    options.retention = Retention::Never;
    options.echo = false;
    options.run_key = Some(RunKey::new(key));
    options
}

/// Lower `workflow`, run it to completion, and inspect the run through its
/// store.
async fn run_workflow(
    rt: &Runtime,
    store: &MemoryRunStore,
    key: &str,
    workflow: &Path,
) -> RunInspection {
    let lowered = rt
        .check(workflow, None, None, &CompileInputs::new())
        .expect("the workflow file loads");
    let graph = lowered
        .graph
        .unwrap_or_else(|| panic!("the workflow lowers: {:?}", lowered.diagnostics));
    let host_run = HostRun::new(graph).with_children(lowered.children);
    let report = host::run_configured(rt, host_run, |_, _| {})
        .await
        .expect("the run completes");
    assert_eq!(
        report.status,
        RunStatus::Success,
        "errors: {:?}; history: {:#?}",
        report.state.errors(),
        report.state.history()
    );
    let logs = store
        .open(&RunKey::new(key), Access::Read)
        .await
        .expect("the run opens for reading");
    inspect::inspect_run(&*logs)
        .await
        .expect("the stored run inspects")
}

/// The `hello` bundle, whose one stage is a prompt, completes on the stub
/// registry with no model, and its record in the memory store says so. The
/// stubbed stages never run a command, but the run still takes its host
/// scope through the plugin.
#[tokio::test]
async fn the_hello_bundle_runs_in_memory_on_the_stub_registry() {
    if host_plugin().is_none() {
        return;
    }
    let root = tempfile::tempdir().expect("a temp dir");
    let bundle = hello_bundle();
    let workflow_text = fs::read_to_string(bundle.join("workflow.fabro"))
        .await
        .expect("the hello workflow is checked in");
    let settings_text = fs::read_to_string(bundle.join("workflow.toml"))
        .await
        .expect("the hello settings are checked in");
    let workflow = install_bundle(root.path(), "hello", &[
        ("workflow.fabro", &workflow_text),
        ("workflow.toml", &settings_text),
    ])
    .await;
    let store = Arc::new(MemoryRunStore::new());
    let rt = petri_attractor_steps::register_stubs(Runtime::standard().frontend(Fabro::new()))
        .store(store.clone())
        .options(run_options(&root.path().join("run"), "hello"));

    let inspection = run_workflow(&rt, &store, "hello", &workflow).await;

    assert!(inspection.complete, "{:?}", inspection.incomplete);
    assert_eq!(inspection.status.as_deref(), Some("success"));
    assert_eq!(inspection.run_key, RunKey::new("hello"));
    assert_eq!(inspection.executions.len(), 1);
}

/// A command-only workflow runs its script on the host sandbox through the
/// real step registry, and its record in the memory store says so.
#[tokio::test]
async fn a_command_workflow_runs_on_the_host_sandbox() {
    if host_plugin().is_none() {
        return;
    }
    let root = tempfile::tempdir().expect("a temp dir");
    let workflow = install_bundle(root.path(), "command", &[
        ("workflow.fabro", COMMAND_WORKFLOW),
        ("workflow.toml", COMMAND_SETTINGS),
    ])
    .await;
    let store = Arc::new(MemoryRunStore::new());
    let rt = petri_attractor_steps::register(Runtime::standard().frontend(Fabro::new()))
        .store(store.clone())
        .options(run_options(&root.path().join("run"), "command"));

    let inspection = run_workflow(&rt, &store, "command", &workflow).await;

    assert!(inspection.complete, "{:?}", inspection.incomplete);
    assert_eq!(inspection.status.as_deref(), Some("success"));
    assert_eq!(inspection.run_key, RunKey::new("command"));
    assert_eq!(inspection.executions.len(), 1);
}
