//! A resume through the engine assembly: a run whose creation a crash cut
//! short starts again from its admitted graphs, and a run whose store
//! failed ends its lifetime without an end of its own.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fabro_petri::admission::AdmittedGraphs;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, Conclusion, Execution, RunError, RunStatus};
use fabro_petri::runtime::RuntimeSpec;
use petri_store::{
    Access, Digest, LogId, MemoryRunStore, OwnerId, Record, RunKey, RunLogs, RunStore, StoreError,
};
use support::{SETTINGS, Silent, admit, all_records, no_questions, run_request};

/// One command stage between start and exit.
const COMMAND: &str = r#"digraph Command {
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="true"]
    start -> say -> exit
}"#;

/// A crash cut the run's creation short: its key is stored, and nothing
/// else. The resume Petri refuses as never started becomes a start from
/// the admitted graphs, under the same key.
#[tokio::test]
async fn a_resume_of_a_run_that_never_started_starts_it_again() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(MemoryRunStore::new());
    drop(
        store
            .open(&RunKey::new("cut-short"), Access::Create {
                owner: OwnerId::new("crashed"),
            })
            .await
            .expect("the key is stored"),
    );
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", COMMAND), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let outcome = engine::run(resume_request(
        "cut-short",
        root.path(),
        graphs,
        store,
        runtime,
    ))
    .await
    .expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
}

/// A write to the run's store fails partway through the run: the lifetime
/// ends with the store's failure, which interrupts the run rather than
/// failing it, and nothing records an end. A resume over the same records
/// finishes the run.
#[tokio::test]
async fn a_failed_store_write_interrupts_the_run_and_a_resume_finishes_it() {
    let root = tempfile::tempdir().expect("a temp dir");
    let memory = Arc::new(MemoryRunStore::new());
    let failing = Arc::new(FailingStore {
        inner:    Arc::clone(&memory),
        log:      LogId::Resources,
        appended: Arc::new(AtomicUsize::new(0)),
    });
    let runtime = RuntimeSpec::default();
    let graphs = || {
        admit(
            &[("workflow.fabro", COMMAND), ("workflow.toml", SETTINGS)],
            Launch::default(),
            &runtime,
        )
    };

    let first = engine::run(run_request(
        "interrupted",
        root.path(),
        graphs(),
        failing,
        runtime.clone(),
        no_questions(Arc::new(Silent)),
    ))
    .await;

    assert!(
        matches!(&first, Err(RunError::StoreFailed(message)) if message.contains("the disk is full")),
        "{first:?}"
    );
    assert!(
        matches!(engine::conclusion(&first), Conclusion::Interrupted { .. }),
        "{first:?}"
    );
    assert!(!finished(&memory).await, "nothing ended the run");

    let outcome = engine::run(resume_request(
        "interrupted",
        root.path(),
        graphs(),
        Arc::clone(&memory) as Arc<dyn RunStore>,
        runtime,
    ))
    .await
    .expect("the resumed run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
    assert!(finished(&memory).await, "the resume ended the run");
}

/// Whether the interrupted run's records hold Petri's own finish.
async fn finished(store: &MemoryRunStore) -> bool {
    all_records(store, "interrupted")
        .await
        .iter()
        .any(|record| record["body"]["event"] == "run.finished")
}

/// A resume of the run over `store`, with the admitted graphs to start it
/// from should its creation have been cut short.
fn resume_request(
    run_id: &str,
    run_dir: &std::path::Path,
    graphs: AdmittedGraphs,
    store: Arc<dyn RunStore>,
    runtime: RuntimeSpec,
) -> engine::RunRequest {
    let mut request = run_request(
        run_id,
        run_dir,
        graphs,
        store,
        runtime,
        no_questions(Arc::new(Silent)),
    );
    let Execution::Start(graphs) = request.execution else {
        unreachable!("run_request builds a start");
    };
    request.execution = Execution::Resume(graphs);
    request
}

/// The in-memory store, failing its first append to `log` as a full disk
/// would, before anything is stored.
struct FailingStore {
    inner:    Arc<MemoryRunStore>,
    log:      LogId,
    appended: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RunStore for FailingStore {
    async fn open(&self, key: &RunKey, access: Access) -> Result<Arc<dyn RunLogs>, StoreError> {
        Ok(Arc::new(FailingLogs {
            inner:    self.inner.open(key, access).await?,
            log:      self.log,
            appended: Arc::clone(&self.appended),
        }))
    }
}

struct FailingLogs {
    inner:    Arc<dyn RunLogs>,
    log:      LogId,
    appended: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RunLogs for FailingLogs {
    fn locator(&self) -> String {
        self.inner.locator()
    }

    async fn append(&self, log: &LogId, records: &[Record]) -> Result<(), StoreError> {
        if *log == self.log && self.appended.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(StoreError::backend(
                self.locator(),
                "append",
                "the disk is full",
            ));
        }
        self.inner.append(log, records).await
    }

    async fn read(&self, log: &LogId) -> Result<Vec<Record>, StoreError> {
        self.inner.read(log).await
    }

    async fn put_blob(&self, bytes: &[u8]) -> Result<Digest, StoreError> {
        self.inner.put_blob(bytes).await
    }

    async fn get_blob(&self, digest: Digest) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_blob(digest).await
    }
}
