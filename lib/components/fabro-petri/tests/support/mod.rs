//! What the adapter tests share: the host plugin lookup, a bundle admitted
//! through `check`, a run request over the engine assembly, and the run's
//! records read back from its store.

#![allow(
    dead_code,
    reason = "each test file uses the part of the support it needs"
)]

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fabro_petri::admission::AdmittedGraphs;
use fabro_petri::check::{self, Bundle, CheckRequest, Launch};
use fabro_petri::controls::RunControls;
use fabro_petri::engine::{Execution, RunRequest};
use fabro_petri::interview::{Approval, FabroInterviewer, QuestionNotice, QuestionSink};
use fabro_petri::runtime::RuntimeSpec;
use fabro_types::SandboxProviderKind;
use petri_execution::inspect;
use petri_store::{Access, LogId, RunKey, RunStore};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

const HOST_PLUGIN: &str = "sandbox-driver-host";
const HOST_PLUGIN_OVERRIDE: &str = "PETRI_SANDBOX_HOST_PLUGIN";
const REQUIRE_ENV: &str = "FABRO_REQUIRE_SANDBOX_PLUGINS";

pub(crate) const POLL: Duration = Duration::from_millis(10);
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// The host plugin as Petri's lookup finds it: the override variable, else
/// the executable on `PATH`. `None`, after saying so, when the test should
/// skip; a panic when the environment forbids a skip.
#[expect(
    clippy::disallowed_methods,
    reason = "the tests locate the plugin executable through the process environment"
)]
#[expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]
pub(crate) fn host_plugin() -> Option<PathBuf> {
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

/// The `.fabro/workflows/hello` bundle checked into this repository.
pub(crate) fn hello_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.fabro/workflows/hello")
}

pub(crate) const SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

pub(crate) fn bundle(files: &[(&str, &str)]) -> Bundle {
    Bundle {
        files:        files
            .iter()
            .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
            .collect(),
        entrypoint:   "workflow.fabro".to_string(),
        project_toml: None,
    }
}

/// Admit a bundle as the create handler does, with the given launch.
pub(crate) fn admit(
    files: &[(&str, &str)],
    launch: Launch,
    runtime: &RuntimeSpec,
) -> AdmittedGraphs {
    let request = CheckRequest {
        bundle: bundle(files),
        inputs: BTreeMap::new(),
        vars: BTreeMap::new(),
        launch,
        runtime: runtime.clone(),
        unbound_is_warning: false,
    };
    let admitted = check::check(&request)
        .unwrap_or_else(|error| panic!("the workflow is admitted: {error:?}"));
    AdmittedGraphs {
        graph:    admitted.graph,
        children: admitted.children,
    }
}

/// A run request over the engine assembly, on the host sandbox, with a
/// fresh cancel token and nothing installed beyond the interviewer.
pub(crate) fn run_request(
    run_id: &str,
    run_dir: &Path,
    graphs: AdmittedGraphs,
    store: Arc<dyn RunStore>,
    runtime: RuntimeSpec,
    interviewer: FabroInterviewer,
) -> RunRequest {
    RunRequest {
        run_id: run_id.to_string(),
        run_dir: run_dir.to_path_buf(),
        execution: Execution::Start(graphs),
        store,
        runtime,
        provider: SandboxProviderKind::LOCAL,
        cancel: CancellationToken::new(),
        controls: RunControls::new(),
        observers: vec![interviewer.observer()],
        interviewer: Arc::new(interviewer),
        secrets: None,
        blobs: None,
        hooks: None,
    }
}

/// An interviewer whose answers nobody delivers, for runs that ask nothing.
pub(crate) fn no_questions(sink: Arc<dyn QuestionSink>) -> FabroInterviewer {
    FabroInterviewer::new(
        Arc::new(fabro_interview::ControlInterviewer::new()),
        Approval::Prompt,
    )
    .with_sink(sink)
}

/// A sink that drops every notice.
pub(crate) struct Silent;

#[async_trait::async_trait]
impl QuestionSink for Silent {
    async fn post(&self, _notice: QuestionNotice) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Every record of every log of a stored run, as JSON, in log order.
pub(crate) async fn all_records(store: &dyn RunStore, run_id: &str) -> Vec<serde_json::Value> {
    let logs = store
        .open(&RunKey::new(run_id), Access::Read)
        .await
        .expect("the run opens for reading");
    let inspection = inspect::inspect_run(&*logs)
        .await
        .expect("the stored run inspects");
    let mut ids = vec![LogId::Coordinator, LogId::Resources];
    ids.extend(
        inspection
            .executions
            .iter()
            .map(|execution| LogId::Execution(execution.execution)),
    );
    let mut records = Vec::new();
    for id in ids {
        records.extend(
            logs.read(&id)
                .await
                .expect("the log reads")
                .into_iter()
                .map(|record| record.record),
        );
    }
    records
}

/// Wait until `condition` holds, polling, or fail after [`PATIENCE`].
pub(crate) async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(POLL).await;
    }
}
