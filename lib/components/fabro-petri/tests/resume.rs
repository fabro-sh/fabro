//! A resume through the engine assembly: a run whose creation a crash cut
//! short starts again from its admitted graphs, and a run whose store
//! failed ends its lifetime without an end of its own.

mod support;

use std::sync::Arc;

use fabro_petri::check::Launch;
use fabro_petri::engine::{self, Execution, RunStatus};
use fabro_petri::runtime::RuntimeSpec;
use petri_store::{Access, MemoryRunStore, OwnerId, RunKey, RunStore as _};
use support::{SETTINGS, Silent, admit, no_questions, run_request};

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
    let mut request = run_request(
        "cut-short",
        root.path(),
        graphs,
        store,
        runtime,
        no_questions(Arc::new(Silent)),
    );
    request.execution = match request.execution {
        Execution::Start(graphs) => Execution::Resume(graphs),
        resume @ Execution::Resume(_) => resume,
    };

    let outcome = engine::run(request).await.expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);
}
