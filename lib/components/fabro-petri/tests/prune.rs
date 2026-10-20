//! A finished run's sandboxes deleted through Petri's lease ledger: the
//! host workspace the run kept is removed by its provider and its lease
//! tombstoned in the run's record, a second prune has nothing to do, and a
//! run a live handle holds is refused.
//!
//! The Host scope runs in process without a plugin executable.

#![expect(
    clippy::disallowed_methods,
    reason = "the test reads the run directory with sync std::fs between awaits"
)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::providers::SandboxProviderConfig;
use fabro_petri::prune::{PruneError, PruneRequest, prune};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::{SqliteRunStore, petri};
use fabro_store::test_support;
use fabro_types::SandboxProviderKind;
use support::{Silent, admit, no_questions, run_request};

/// A command-only workflow whose one stage writes a file into its
/// workspace.
const WORKFLOW: &str = r#"digraph Command {
    graph [goal="Leave a file behind"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    write [shape=parallelogram, script="echo kept > kept.txt"]
    start -> write -> exit
}"#;

/// Every file under `root`, relative to it, in path order.
fn files_under(root: &std::path::Path) -> Vec<PathBuf> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("an entry reads").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(
                    path.strip_prefix(root)
                        .expect("a path under the root")
                        .to_path_buf(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The one host workspace under the run directory: the host backend keeps a
/// scope's state under `scopes/<id>` and its workspace under `work` there.
fn workspace(run_dir: &std::path::Path) -> Option<PathBuf> {
    let scopes = run_dir.join("scopes");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&scopes)
        .ok()?
        .map(|entry| entry.expect("an entry reads").path())
        .collect();
    assert!(entries.len() <= 1, "one scope at most: {entries:?}");
    entries.pop().map(|scope| scope.join("work"))
}

/// The state of each lease in the run's resource log, latest record per
/// lease.
async fn lease_states(store: &SqliteRunStore, run_id: &str) -> Vec<String> {
    let logs = petri::RunStore::open(store, &petri::RunKey::new(run_id), petri::Access::Read)
        .await
        .expect("the run opens for reading");
    let records = logs
        .read(&petri::LogId::Resources)
        .await
        .expect("the resource log reads");
    let mut latest = std::collections::BTreeMap::new();
    for record in records {
        let lease = record.record["body"]["lease"].clone();
        let state = record.record["body"]["state"]
            .as_str()
            .expect("a lease state")
            .to_owned();
        latest.insert(lease.to_string(), state);
    }
    latest.into_values().collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_runs_host_workspace_is_deleted_once_and_a_held_run_is_refused() {
    let root = tempfile::tempdir().expect("a temp dir");
    let run_dir = root.path().join("run");
    let pool = test_support::in_memory_pool_with(&[
        fabro_db::BLOBS_MIGRATION_SQL,
        fabro_db::PETRI_RECORDS_MIGRATION_SQL,
    ]);
    let store = Arc::new(SqliteRunStore::new(pool.clone()));
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[
            ("workflow.fabro", WORKFLOW),
            ("workflow.toml", support::SETTINGS),
        ],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "prune",
        &run_dir,
        graphs,
        store.clone(),
        runtime,
        no_questions(Arc::new(Silent)),
    );
    let outcome = engine::run(request).await.expect("the run ends");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");

    // Retention kept the workspace, and its lease is live in the record.
    let kept = workspace(&run_dir).expect("the run's workspace is retained");
    let files = files_under(&kept);
    assert!(
        files
            .iter()
            .any(|file| file.file_name().is_some_and(|name| name == "kept.txt")),
        "the stage's file is in the retained workspace: {files:?}"
    );
    assert_eq!(lease_states(&store, "prune").await, ["stopped"]);

    let logs = petri::RunStore::open(
        store.as_ref(),
        &petri::RunKey::new("prune"),
        petri::Access::Read,
    )
    .await
    .expect("the resources open");
    let resources = logs
        .read(&petri::LogId::Resources)
        .await
        .expect("the resources read");
    let legacy_fingerprint = format!(
        "host:{}",
        run_dir
            .join("host-registry")
            .canonicalize()
            .expect("canonical registry")
            .display()
    );
    let allocating = resources
        .iter()
        .find(|record| {
            record.record["body"]["state"] == "allocating"
                && record.record["body"]["fingerprint"].is_string()
        })
        .expect("the allocation is recorded");
    assert_eq!(allocating.record["body"]["fingerprint"], legacy_fingerprint);
    drop(logs);

    let request = || PruneRequest {
        sandbox:  SandboxProviderConfig::default(),
        run_id:   "prune".to_string(),
        run_dir:  run_dir.clone(),
        store:    store.clone(),
        provider: SandboxProviderKind::LOCAL,
    };
    let report = prune(request()).await.expect("the run prunes");
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.deleted.len(), 1, "{report:?}");
    assert!(
        !kept.exists(),
        "the host provider removed its managed workspace; left: {:?}",
        files_under(&kept)
    );
    assert!(
        run_dir.is_dir(),
        "the run directory itself is the caller's to remove"
    );
    assert_eq!(
        lease_states(&store, "prune").await,
        ["deleted"],
        "the tombstone is in the run's record"
    );

    // A second prune finds only the tombstone.
    let again = prune(request()).await.expect("the run prunes again");
    assert!(again.is_clean() && again.deleted.is_empty(), "{again:?}");
    assert_eq!(again.clean.len(), 1, "{again:?}");

    // A live handle on the run holds its lease; the prune is refused.
    let held = petri::RunStore::open(
        store.as_ref(),
        &petri::RunKey::new("prune"),
        petri::Access::Write {
            owner: petri::OwnerId::new("worker-1"),
        },
    )
    .await
    .expect("the worker takes the run");
    let error = prune(request())
        .await
        .expect_err("a held run is not pruned");
    assert!(matches!(error, PruneError::RunHeld { .. }), "{error}");
    drop(held);
}

#[tokio::test]
async fn a_provider_the_server_has_not_registered_is_refused_before_the_store_is_opened() {
    let store = Arc::new(petri_store::MemoryRunStore::new());
    let error = prune(PruneRequest {
        sandbox: SandboxProviderConfig::default(),
        run_id: "e2b-run".to_string(),
        run_dir: std::env::temp_dir().join("fabro-petri-prune-e2b"),
        store,
        provider: SandboxProviderKind::try_new("e2b").expect("a valid kind"),
    })
    .await
    .expect_err("a provider with no registration cannot be pruned");
    assert!(
        matches!(error, PruneError::UnregisteredProvider { ref message } if message.contains("`e2b`")),
        "{error}"
    );
}
