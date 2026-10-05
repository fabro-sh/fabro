//! Real command-run records for transport and projection tests. No providers
//! or model credentials are used.

use std::sync::Arc;

use petri_execution::host::{self, HostRun};
use petri_execution::inspect;
use petri_runtime::driver::lifecycle::{ExecutionHooks, HookContext, RunFinished};
use petri_runtime::frontend::CompileInputs;
use petri_runtime::ir::FinalizationFailure;
use petri_runtime::{RunOptions, Runtime};
use petri_store::{Access, LogId, MemoryRunStore, Record, RunKey, RunStore as _};
use tokio::sync::{Notify, Semaphore};

use crate::providers::{self, SandboxProviderConfig};

/// A deterministic finalizer gate for testing the committed boundary.
pub struct TestFinalizer {
    pub entered: Notify,
    pub release: Semaphore,
    rejection:   Option<String>,
}

impl TestFinalizer {
    pub fn new(rejection: Option<&str>) -> Self {
        Self {
            entered:   Notify::new(),
            release:   Semaphore::new(0),
            rejection: rejection.map(str::to_string),
        }
    }
}

#[async_trait::async_trait]
impl ExecutionHooks for TestFinalizer {
    fn requires_run_finalization(&self) -> bool {
        true
    }

    async fn finalize_run(
        &self,
        _: &HookContext,
        _: RunFinished,
    ) -> Result<(), FinalizationFailure> {
        self.entered.notify_one();
        self.release
            .acquire()
            .await
            .expect("the gate stays open")
            .forget();
        self.rejection.as_ref().map_or(Ok(()), |message| {
            Err(FinalizationFailure::new("publish_failed", message.clone()))
        })
    }
}

/// The command-only runtime installed with a test finalizer.
pub fn test_runtime(finalizer: Arc<TestFinalizer>) -> Runtime {
    petri_attractor_steps::register(providers::standard_runtime(
        &SandboxProviderConfig::default(),
    ))
    .frontend(petri_frontend_fabro::Fabro::new())
    .hooks(finalizer)
}

/// Logs and blobs of a real command run, for authenticated worker transport
/// tests. The caller can send the coordinator prefix before its final record.
pub struct TestRunRecords {
    pub logs:  Vec<(LogId, Vec<Record>)>,
    pub blobs: Vec<Vec<u8>>,
}

pub async fn test_run_records(
    run_id: fabro_types::RunId,
    rejection: Option<&str>,
) -> TestRunRecords {
    let root = tempfile::tempdir().expect("the fixture has an isolated directory");
    let workflow = root.path().join("workflow.fabro");
    tokio::fs::write(
        &workflow,
        r#"digraph Finalization {
        graph [goal="Check required publication"]
        start [shape=Mdiamond]
        work [shape=parallelogram, script="echo completed"]
        exit [shape=Msquare]
        start -> work -> exit
    }"#,
    )
    .await
    .expect("the fixture workflow writes");
    tokio::fs::write(
        root.path().join("workflow.toml"),
        "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
    )
    .await
    .expect("the settings write");
    let finalizer = Arc::new(TestFinalizer::new(rejection));
    finalizer.release.add_permits(1);
    let store = Arc::new(MemoryRunStore::new());
    let key = RunKey::new(run_id.to_string());
    let mut options = RunOptions::new(root.path().join("run"));
    options.run_key = Some(key.clone());
    options.echo = false;
    let runtime = test_runtime(finalizer)
        .store(store.clone())
        .options(options);
    let checked = runtime
        .check(&workflow, None, None, &CompileInputs::new())
        .expect("the fixture compiles");
    host::run_configured(
        &runtime,
        HostRun::new(checked.graph.expect("the fixture is valid")).with_children(checked.children),
        |_, _| {},
    )
    .await
    .expect("the fixture completes");
    let logs = store
        .open(&key, Access::Read)
        .await
        .expect("the fixture reads");
    let inspection = inspect::inspect_run(&*logs)
        .await
        .expect("the fixture inspects");
    let mut records = vec![(
        LogId::Coordinator,
        logs.read(&LogId::Coordinator)
            .await
            .expect("coordinator reads"),
    )];
    for execution in inspection.executions {
        let id = LogId::Execution(execution.execution);
        records.push((id.clone(), logs.read(&id).await.expect("execution reads")));
    }
    let mut blobs = Vec::new();
    for graph in inspection.graphs {
        blobs.push(
            logs.get_blob(graph)
                .await
                .expect("graph reads")
                .expect("graph exists"),
        );
    }
    TestRunRecords {
        logs: records,
        blobs,
    }
}
