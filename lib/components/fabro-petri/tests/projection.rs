//! The projection of a Petri run: the view built live, as the run appends
//! its records, equals the view rebuilt from the records alone; a view that
//! missed its wake-ups catches up on the next signal; a crash between the
//! record commit and the view transaction is recovered by applying only the
//! missing suffix; two projectors over one store agree over nested child
//! executions; a torn tail holds the view where it stands; and a pass over
//! a live run costs its new records, with the cache that makes it so
//! dropped at a restart, after the idle period and at the run's finish.
//!
//! Every run here takes its scope's environment through the sandbox-driver
//! host plugin, so the tests skip, and say why, when the executable is not
//! found, unless `FABRO_REQUIRE_SANDBOX_PLUGINS` is set.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests locate the plugin executable through the process environment"
)]
#![expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]

mod support;

use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fabro_db::DbPool;
use fabro_interview::ControlInterviewer;
use fabro_petri::blobs::{Blobs, RunBlobs};
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus as EngineRunStatus};
use fabro_petri::interview::{Approval, FabroInterviewer};
use fabro_petri::projector::{self, Projector};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::{SqliteRunStore, test_support as petri_support};
use fabro_store::platform_records::{
    PlatformRecord, PlatformRecordStore, RunCreatedRecord, RunLifecycleKind, RunLifecycleRecord,
};
use fabro_store::{BlobStore, test_support};
use fabro_types::{
    BlobHash, PetriAdmission, PetriGraphRef, RunId, RunStatus, StageHandler, StageId, StageState,
    test_support as types_support,
};
use petri_execution::host::{self, HostRun};
use petri_frontend_fabro::Fabro;
use petri_runtime::executor::Retention;
use petri_runtime::frontend::CompileInputs;
use petri_runtime::ir::RunStatus as PetriRunStatus;
use petri_runtime::{RunOptions, Runtime};
use petri_store::{RunKey, RunStore};
use tokio::fs;
use tokio::time::sleep;

const HOST_PLUGIN: &str = "sandbox-driver-host";
const HOST_PLUGIN_OVERRIDE: &str = "PETRI_SANDBOX_HOST_PLUGIN";
const REQUIRE_ENV: &str = "FABRO_REQUIRE_SANDBOX_PLUGINS";

const COMMAND_WORKFLOW: &str = r#"digraph Command {
    graph [goal="Run one command"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="echo hello from petri"]
    start -> say -> exit
}"#;

/// Two branches, each a command, joined by a fan-in.
const PARALLEL_WORKFLOW: &str = r#"digraph Parallel {
    graph [goal="Run two branches"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    fork [shape=component]
    a [shape=parallelogram, script="echo a"]
    b [shape=parallelogram, script="echo b"]
    merge [shape=tripleoctagon]
    report [shape=parallelogram, script="echo done"]
    start -> fork
    fork -> a
    fork -> b
    a -> merge
    b -> merge
    merge -> report -> exit
}"#;

const SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

/// One yes/no gate whose branches leave a marker file each.
fn gate_workflow(markers: &Path, gate_attrs: &str) -> String {
    format!(
        r#"digraph Gate {{
    graph [goal="Ask once"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    gate [shape=hexagon, label="Go?", question_type="yes_no"{gate_attrs}]
    yes [shape=parallelogram, script="touch {dir}/yes"]
    no [shape=parallelogram, script="touch {dir}/no"]
    start -> gate
    gate -> yes [label="[Y] Yes"]
    gate -> no [label="[N] No"]
    yes -> exit
    no -> exit
}}"#,
        dir = markers.display()
    )
}

/// A gate after a stage with a response, whose affirmative edge says what
/// choosing it means and shows a sample: the facts the interview dock
/// shows beside the choices.
fn described_gate_workflow(markers: &Path) -> String {
    format!(
        r#"digraph Gate {{
    graph [goal="Ask with context"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    plan [shape=parallelogram, output_schema="routing", script="echo '{{\"outcome\": \"succeeded\", \"context_updates\": {{\"last_stage\": \"plan\", \"response.plan\": \"Ship the fix in one commit.\"}}}}'"]
    gate [shape=hexagon, label="Deploy?", timeout="1500ms", human.default_choice="no"]
    yes [shape=parallelogram, script="touch {dir}/yes"]
    no [shape=parallelogram, script="touch {dir}/no"]
    start -> plan -> gate
    gate -> yes [label="[Y] Yes", "human.description"="Merge and deploy to production", "human.preview"="deploy --prod"]
    gate -> no [label="[N] No"]
    yes -> exit
    no -> exit
}}"#,
        dir = markers.display()
    )
}

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

/// A fresh in-memory database with every table the projection touches.
fn pool() -> DbPool {
    test_support::in_memory_pool_with(&[
        fabro_db::BLOBS_MIGRATION_SQL,
        fabro_db::RUNS_MIGRATION_SQL,
        fabro_db::DROP_RUN_EVENTS_MIGRATION_SQL,
        fabro_db::PETRI_RECORDS_MIGRATION_SQL,
        fabro_db::PETRI_PROJECTION_MIGRATION_SQL,
    ])
}

fn hello_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.fabro/workflows/hello")
}

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

fn run_options(run_dir: &Path, run_id: RunId) -> RunOptions {
    let mut options = RunOptions::new(run_dir);
    options.grace = Duration::from_secs(2);
    options.retention = Retention::Never;
    options.echo = false;
    options.run_key = Some(RunKey::new(run_id.to_string()));
    options
}

/// The run's `run.created` platform record, as the create handler writes it,
/// and the `running` lifecycle record the execute path writes.
async fn create_run(pool: &DbPool, run_id: RunId, goal: &str) {
    let store = PlatformRecordStore::new(pool.clone());
    let mut spec = types_support::test_run_spec();
    spec.run_id = run_id;
    spec.admission = PetriAdmission {
        graph:    PetriGraphRef {
            blob:   BlobHash::new(b"graph"),
            digest: "digest".to_string(),
        },
        children: Vec::new(),
    };
    store
        .append(
            &run_id,
            &PlatformRecord::RunCreated(RunCreatedRecord {
                spec,
                title: Some(goal.to_string()),
                parent_id: None,
                retried_from: None,
                web_url: None,
            }),
            None,
        )
        .await
        .expect("the created record stores");
    for (transition, status) in [
        (RunLifecycleKind::Runnable, RunStatus::Runnable),
        (RunLifecycleKind::Starting, RunStatus::Starting),
        (RunLifecycleKind::Running, RunStatus::Running),
    ] {
        store
            .append(
                &run_id,
                &PlatformRecord::RunLifecycle(
                    RunLifecycleRecord::new(transition).with_status(status),
                ),
                None,
            )
            .await
            .expect("the lifecycle record stores");
    }
}

/// Run `workflow` to completion on the real registry over `store`.
async fn run_workflow(
    store: Arc<dyn RunStore>,
    blobs: Arc<dyn Blobs>,
    run_dir: &Path,
    run_id: RunId,
    workflow: &Path,
    stubs: bool,
) {
    let runtime = Runtime::standard().frontend(Fabro::new());
    let runtime = if stubs {
        petri_attractor_steps::register_stubs(runtime)
    } else {
        petri_attractor_steps::register(runtime)
    };
    // A large stage value goes to Fabro's blob table, as it does under the
    // worker and the server.
    let runtime = runtime.capability(RunBlobs::output_store(blobs));
    let rt = runtime.store(store).options(run_options(run_dir, run_id));
    let lowered = rt
        .check(workflow, None, None, &CompileInputs::new())
        .expect("the workflow file loads");
    let graph = lowered
        .graph
        .unwrap_or_else(|| panic!("the workflow lowers: {:?}", lowered.diagnostics));
    let host_run = HostRun::new(graph).with_children(lowered.children);
    let report = host::run_configured(&rt, host_run, |_, _| {})
        .await
        .expect("the run completes");
    assert_eq!(
        report.status,
        PetriRunStatus::Success,
        "errors: {:?}",
        report.state.errors()
    );
}

/// A scenario: its bundle installed, its run created in the database.
struct Scenario {
    pool:     DbPool,
    run_id:   RunId,
    workflow: PathBuf,
    run_dir:  PathBuf,
    stubs:    bool,
    _root:    tempfile::TempDir,
}

async fn scenario(name: &str, files: &[(&str, &str)], stubs: bool) -> Scenario {
    let root = tempfile::tempdir().expect("a temp dir");
    let workflow = install_bundle(root.path(), name, files).await;
    let pool = pool();
    let run_id = RunId::new();
    create_run(&pool, run_id, name).await;
    Scenario {
        pool,
        run_id,
        workflow,
        run_dir: root.path().join("run"),
        stubs,
        _root: root,
    }
}

async fn hello_scenario() -> Scenario {
    let bundle = hello_bundle();
    let workflow = fs::read_to_string(bundle.join("workflow.fabro"))
        .await
        .expect("the hello workflow is checked in");
    let settings = fs::read_to_string(bundle.join("workflow.toml"))
        .await
        .expect("the hello settings are checked in");
    scenario(
        "hello",
        &[("workflow.fabro", &workflow), ("workflow.toml", &settings)],
        true,
    )
    .await
}

async fn command_scenario() -> Scenario {
    scenario(
        "command",
        &[
            ("workflow.fabro", COMMAND_WORKFLOW),
            ("workflow.toml", SETTINGS),
        ],
        false,
    )
    .await
}

/// A command whose output is above Petri's offload threshold.
const LARGE_OUTPUT_WORKFLOW: &str = r#"digraph Large {
    graph [goal="Print a lot"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    big [shape=parallelogram, script="yes xxxxxxxxxxxxxxxx | head -n 8000"]
    start -> big -> exit
}"#;

async fn large_output_scenario() -> Scenario {
    scenario(
        "large",
        &[
            ("workflow.fabro", LARGE_OUTPUT_WORKFLOW),
            ("workflow.toml", SETTINGS),
        ],
        false,
    )
    .await
}

async fn parallel_scenario() -> Scenario {
    scenario(
        "parallel",
        &[
            ("workflow.fabro", PARALLEL_WORKFLOW),
            ("workflow.toml", SETTINGS),
        ],
        false,
    )
    .await
}

/// Run the scenario live: every append signals the projector, and the view
/// settles before the run is compared with its rebuild.
async fn run_live(scenario: &Scenario) -> Arc<Projector> {
    let projector = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    projector.signal(scenario.run_id);
    let store = projector.observe_store(Arc::new(SqliteRunStore::new(scenario.pool.clone())));
    run_workflow(
        store,
        Arc::new(BlobStore::new(scenario.pool.clone())),
        &scenario.run_dir,
        scenario.run_id,
        &scenario.workflow,
        scenario.stubs,
    )
    .await;
    projector.settle(scenario.run_id).await;
    projector
}

/// Run the scenario with no projector attached: the records land and
/// nothing wakes the view.
async fn run_unobserved(scenario: &Scenario) {
    run_workflow(
        Arc::new(SqliteRunStore::new(scenario.pool.clone())),
        Arc::new(BlobStore::new(scenario.pool.clone())),
        &scenario.run_dir,
        scenario.run_id,
        &scenario.workflow,
        scenario.stubs,
    )
    .await;
}

/// Every path where two JSON values differ, with both sides.
fn diff_json(path: &str, left: &serde_json::Value, right: &serde_json::Value) -> Vec<String> {
    use serde_json::Value;
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys: BTreeSet<&String> = left.keys().chain(right.keys()).collect();
            keys.into_iter()
                .flat_map(|key| {
                    diff_json(
                        &format!("{path}/{key}"),
                        left.get(key).unwrap_or(&Value::Null),
                        right.get(key).unwrap_or(&Value::Null),
                    )
                })
                .collect()
        }
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => left
            .iter()
            .zip(right)
            .enumerate()
            .flat_map(|(index, (left, right))| diff_json(&format!("{path}[{index}]"), left, right))
            .collect(),
        _ if left == right => Vec::new(),
        _ => vec![format!("{path}: live {left} != rebuilt {right}")],
    }
}

fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("the value serializes")
}

/// The stored view equals the view rebuilt from the records alone: the
/// projection, the positions and the delivery sequence.
async fn assert_view_equals_rebuild(pool: &DbPool, run_id: RunId) {
    let stored = projector::stored_projection(pool, run_id)
        .await
        .expect("the stored projection reads")
        .expect("the run has a stored projection");
    let (stored_positions, stored_stream_seq) = projector::stored_positions(pool, run_id)
        .await
        .expect("the positions read")
        .expect("the run has positions");
    let (rebuilt, positions, stream_seq) = projector::rebuild(pool, pool, run_id)
        .await
        .expect("the run rebuilds");
    let rebuilt = rebuilt.expect("the rebuild has a projection");
    let differences = diff_json("", &json(&stored), &json(&rebuilt));
    assert!(
        differences.is_empty(),
        "live view differs from the rebuild at:\n{}",
        differences.join("\n")
    );
    let mut stored_positions = stored_positions;
    let mut positions = positions;
    stored_positions.petri.sort();
    positions.petri.sort();
    assert_eq!(stored_positions, positions);
    assert_eq!(stored_stream_seq, stream_seq);
    let stream = projector::stored_stream(pool, run_id)
        .await
        .expect("the stream reads");
    let seqs: Vec<u64> = stream.iter().map(|(seq, _, _)| *seq).collect();
    assert_eq!(
        seqs,
        (1..=stream_seq).collect::<Vec<_>>(),
        "contiguous stream"
    );
}

async fn stage_states(pool: &DbPool, run_id: RunId) -> Vec<(String, StageState)> {
    let stored = projector::stored_projection(pool, run_id)
        .await
        .expect("the stored projection reads")
        .expect("the run has a stored projection");
    stored
        .iter_stages()
        .map(|(id, stage)| (id.to_string(), stage.state))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hello_bundle_projects_live_as_it_rebuilds() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = hello_scenario().await;
    run_live(&scenario).await;
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
    let stored = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    assert!(
        matches!(stored.status, RunStatus::Succeeded { .. }),
        "{:?}",
        stored.status
    );
    assert!(stored.conclusion.is_some(), "the run concluded");
    let states = stage_states(&scenario.pool, scenario.run_id).await;
    assert!(
        states
            .iter()
            .any(|(label, state)| label.starts_with("start@") && *state == StageState::Succeeded),
        "{states:?}"
    );
}

/// A command's offloaded output reaches the view as its `blob://`
/// reference, never as the bytes the live log accumulated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_output_projects_as_its_blob_reference() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = large_output_scenario().await;
    run_live(&scenario).await;
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
    let stored = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    let big = stored
        .stage(&StageId::new("big", 1))
        .expect("the command stage is shown");
    let output = big.output.as_deref().expect("the stage has an output");
    assert!(
        fabro_types::parse_blob_ref(output).is_some(),
        "the output is a blob reference: {} bytes, {}",
        output.len(),
        &output[..output.len().min(80)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_workflow_projects_live_as_it_rebuilds() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = command_scenario().await;
    run_live(&scenario).await;
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
    let stored = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    let say = stored
        .stage(&StageId::new("say", 1))
        .expect("the command stage is shown");
    assert_eq!(say.state, StageState::Succeeded);
    assert_eq!(say.handler, Some(StageHandler::Command));
    assert!(
        say.output
            .as_deref()
            .is_some_and(|output| output.contains("hello from petri")),
        "{:?}",
        say.output
    );
    assert!(say.timing.is_some());
    let states = stage_states(&scenario.pool, scenario.run_id).await;
    assert_eq!(states.len(), 3, "start, say, exit: {states:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parallel_workflow_projects_its_branches_as_child_executions() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = parallel_scenario().await;
    run_live(&scenario).await;
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
    let stored = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    let fork = StageId::new("fork", 1);
    for branch in ["a", "b"] {
        let stage = stored
            .stage(&StageId::new(branch, 1))
            .unwrap_or_else(|| panic!("branch {branch} is a stage"));
        assert_eq!(stage.state, StageState::Succeeded);
        let branch_id = stage
            .parallel_branch_id
            .as_ref()
            .unwrap_or_else(|| panic!("branch {branch} is grouped under the fork"));
        assert_eq!(branch_id.group(), &fork);
    }
    let fork_stage = stored.stage(&fork).expect("the fork is a stage");
    let results = fork_stage
        .parallel_results
        .as_ref()
        .expect("the fork carries its branch results");
    assert_eq!(results.len(), 2, "{results:?}");
    let labels: Vec<String> = stored.iter_stages().map(|(id, _)| id.to_string()).collect();
    assert!(
        !labels.iter().any(|label| label.contains("fan_in")),
        "synthetic nodes stay off the list: {labels:?}"
    );
}

/// The projector is not signalled for any append; one signal at the end
/// folds everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_wake_ups_are_caught_up_by_the_next_signal() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = command_scenario().await;
    run_unobserved(&scenario).await;
    assert!(
        projector::stored_projection(&scenario.pool, scenario.run_id)
            .await
            .expect("reads")
            .is_none(),
        "nothing woke the view"
    );
    let projector = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    projector.signal(scenario.run_id);
    projector.settle(scenario.run_id).await;
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
    let report = projector
        .project_run(scenario.run_id)
        .await
        .expect("a pass over a caught-up view");
    assert!(report.skipped, "nothing is left to fold: {report:?}");
    assert!(report.health.complete, "{:?}", report.health.incomplete);
}

/// The same, through the startup pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_startup_pass_catches_up_a_view_nobody_signalled() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = command_scenario().await;
    run_unobserved(&scenario).await;
    let projector = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    let report = projector
        .startup_pass()
        .await
        .expect("the startup pass runs");
    assert_eq!((report.runs, report.projected), (1, 1));
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
    let again = projector
        .startup_pass()
        .await
        .expect("a second startup pass");
    assert_eq!((again.runs, again.projected), (1, 0), "nothing left to do");
}

/// Passes over one run never interleave: the startup pass and a signalled
/// pass racing over the same run commit one stream, contiguous and without
/// a duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_passes_over_one_run_commit_one_contiguous_stream() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = parallel_scenario().await;
    run_unobserved(&scenario).await;
    let projector = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    let mut passes = Vec::new();
    for _ in 0..4 {
        let projector = Arc::clone(&projector);
        let run_id = scenario.run_id;
        passes.push(tokio::spawn(
            async move { projector.project_run(run_id).await },
        ));
    }
    projector.signal(scenario.run_id);
    projector
        .startup_pass()
        .await
        .expect("the startup pass runs");
    for pass in passes {
        pass.await
            .expect("the pass task joins")
            .expect("a concurrent pass commits or skips");
    }
    projector.settle(scenario.run_id).await;
    assert_view_equals_rebuild(&scenario.pool, scenario.run_id).await;
}

/// Every Petri record of the run, as `(log, seq, recorded_at, record_json)`.
async fn petri_rows(pool: &DbPool, run_id: RunId) -> Vec<(String, i64, i64, String)> {
    sqlx::query_as(
        "SELECT log, seq, recorded_at, record_json FROM petri_records WHERE run_id = ? ORDER BY \
         log, seq",
    )
    .bind(run_id.to_string())
    .fetch_all(pool)
    .await
    .expect("the records read")
}

async fn insert_petri_row(pool: &DbPool, run_id: RunId, row: &(String, i64, i64, String)) {
    sqlx::query(
        "INSERT INTO petri_records (run_id, log, seq, recorded_at, record_json) VALUES (?, ?, ?, \
         ?, ?)",
    )
    .bind(run_id.to_string())
    .bind(&row.0)
    .bind(row.1)
    .bind(row.2)
    .bind(&row.3)
    .execute(pool)
    .await
    .expect("the record inserts");
}

/// A copy of the run in a fresh database: its blobs, its Petri run row and
/// its platform records, but none of its Petri records yet.
async fn copy_run_without_records(source: &DbPool, run_id: RunId) -> DbPool {
    let target = pool();
    let blobs: Vec<(String, Vec<u8>)> = sqlx::query_as("SELECT hash, data FROM blobs")
        .fetch_all(source)
        .await
        .expect("the blobs read");
    for (hash, data) in blobs {
        sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
            .bind(hash)
            .bind(data)
            .execute(&target)
            .await
            .expect("the blob inserts");
    }
    sqlx::query("INSERT INTO petri_runs (run_id, created_at_ms, owner_id, acquired_at_ms) VALUES (?, 0, NULL, NULL)")
        .bind(run_id.to_string())
        .execute(&target)
        .await
        .expect("the run row inserts");
    let platform: Vec<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT seq, recorded_at, kind, record_json FROM platform_records WHERE run_id = ? ORDER \
         BY seq",
    )
    .bind(run_id.to_string())
    .fetch_all(source)
    .await
    .expect("the platform records read");
    for (seq, recorded_at, kind, record_json) in platform {
        sqlx::query(
            "INSERT INTO platform_records (run_id, seq, recorded_at, kind, record_json) VALUES \
             (?, ?, ?, ?, ?)",
        )
        .bind(run_id.to_string())
        .bind(seq)
        .bind(recorded_at)
        .bind(kind)
        .bind(record_json)
        .execute(&target)
        .await
        .expect("the platform record inserts");
    }
    target
}

/// The records commit in two halves and the view runs between them, then
/// the process dies before the view catches the second half: the rebuilt
/// view applies only the suffix, with the positions and the delivery
/// sequence continuing from where the committed view stood.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_between_the_record_commit_and_the_view_applies_only_the_suffix() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = parallel_scenario().await;
    run_unobserved(&scenario).await;
    let rows = petri_rows(&scenario.pool, scenario.run_id).await;
    let replayed = copy_run_without_records(&scenario.pool, scenario.run_id).await;

    // The first half of every log: a prefix per log, the coordinator log
    // short of its finish.
    let mut first: Vec<&(String, i64, i64, String)> = Vec::new();
    let mut second: Vec<&(String, i64, i64, String)> = Vec::new();
    for row in &rows {
        let head = rows
            .iter()
            .filter(|other| other.0 == row.0)
            .map(|other| other.1)
            .max()
            .expect("the log has a head");
        if row.1 <= head / 2 {
            first.push(row);
        } else {
            second.push(row);
        }
    }
    for row in &first {
        insert_petri_row(&replayed, scenario.run_id, row).await;
    }
    let before = Projector::new(replayed.clone(), replayed.clone());
    let pass = before
        .project_run(scenario.run_id)
        .await
        .expect("the first pass commits");
    assert!(!pass.skipped);
    assert!(!pass.health.complete, "the run has not finished");
    let (positions_before, stream_before) = projector::stored_positions(&replayed, scenario.run_id)
        .await
        .expect("reads")
        .expect("positions");
    assert_eq!(pass.stream_seq, stream_before);
    let stream_rows_before = projector::stored_stream(&replayed, scenario.run_id)
        .await
        .expect("reads")
        .len();

    // The rest of the records commit; the view transaction never runs.
    for row in &second {
        insert_petri_row(&replayed, scenario.run_id, row).await;
    }
    before.fail_before_view();
    let crashed = before.project_run(scenario.run_id).await;
    assert!(
        matches!(crashed, Err(projector::ProjectError::Injected)),
        "{crashed:?}"
    );
    assert_eq!(
        projector::stored_positions(&replayed, scenario.run_id)
            .await
            .expect("reads")
            .expect("positions"),
        (positions_before.clone(), stream_before),
        "the crash left the committed view alone"
    );

    // A new projector, as a restarted server builds one.
    let after = Projector::new(replayed.clone(), replayed.clone());
    let report = after.startup_pass().await.expect("the restart catches up");
    assert_eq!((report.runs, report.projected), (1, 1));
    let (positions_after, stream_after) = projector::stored_positions(&replayed, scenario.run_id)
        .await
        .expect("reads")
        .expect("positions");
    let stream_rows_after = projector::stored_stream(&replayed, scenario.run_id)
        .await
        .expect("reads");
    // Only the suffix was applied: the stream grew by the suffix's events,
    // numbered on from the committed sequence, and every earlier row stayed.
    assert_eq!(
        stream_rows_after.len(),
        stream_rows_before + usize::try_from(stream_after - stream_before).expect("a small count")
    );
    assert!(stream_after > stream_before);
    assert_eq!(
        stream_rows_after[stream_rows_before].0,
        stream_before + 1,
        "the suffix starts right after the committed sequence"
    );
    for held in &positions_before.petri {
        let now = positions_after
            .petri
            .iter()
            .find(|after| after.source == held.source)
            .expect("a held log is still held");
        assert!(now >= held, "{now:?} >= {held:?}");
    }
    assert_eq!(positions_after.platform_seq, positions_before.platform_seq);
    assert_view_equals_rebuild(&replayed, scenario.run_id).await;
    // And the copy agrees with the run projected in one go over the source.
    let source = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    source.startup_pass().await.expect("the source projects");
    let whole = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    let pieced = projector::stored_projection(&replayed, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    assert_eq!(json(&whole), json(&pieced));
}

/// Two projectors over one store, one after the other, over a run with
/// child executions: the second continues where the first stopped and both
/// agree with a projector that saw the run whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_projector_agrees_over_nested_child_executions() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = parallel_scenario().await;
    run_unobserved(&scenario).await;
    let rows = petri_rows(&scenario.pool, scenario.run_id).await;
    assert!(
        rows.iter()
            .filter(|row| row.0.starts_with("execution "))
            .map(|row| &row.0)
            .collect::<BTreeSet<_>>()
            .len()
            >= 3,
        "the parallel run has child executions: {:?}",
        rows.iter().map(|row| &row.0).collect::<BTreeSet<_>>()
    );
    let staged = copy_run_without_records(&scenario.pool, scenario.run_id).await;
    let first = Projector::new(staged.clone(), staged.clone());
    // The parent execution and the coordinator log up to the first child's
    // declaration go in first; a restart then sees the children.
    let (early, late): (Vec<_>, Vec<_>) = rows
        .iter()
        .partition(|row| row.0 == "execution 0" || (row.0 == "coordinator" && row.1 < 6));
    for row in &early {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    first
        .startup_pass()
        .await
        .expect("the first projector passes");
    for row in &late {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    drop(first);
    let second = Projector::new(staged.clone(), staged.clone());
    second
        .startup_pass()
        .await
        .expect("the second projector passes");
    assert_view_equals_rebuild(&staged, scenario.run_id).await;

    let whole = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    whole.startup_pass().await.expect("the source projects");
    let one_go = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    let restarted = projector::stored_projection(&staged, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    assert_eq!(json(&one_go), json(&restarted));
    let states = stage_states(&staged, scenario.run_id).await;
    assert!(
        states.iter().any(|(label, _)| label == "a@1")
            && states.iter().any(|(label, _)| label == "b@1"),
        "{states:?}"
    );
}

/// A record at seq n+2 of an execution log, past a gap: Petri cannot read
/// the log, the view does not advance past what it held, and the run is
/// reported incomplete with the reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_torn_tail_holds_the_view_and_reports_the_run_incomplete() {
    if host_plugin().is_none() {
        return;
    }
    let scenario = command_scenario().await;
    run_unobserved(&scenario).await;
    let projector = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    let clean = projector
        .project_run(scenario.run_id)
        .await
        .expect("the clean pass commits");
    assert!(clean.health.complete, "{:?}", clean.health.incomplete);
    let (positions, stream_seq) = projector::stored_positions(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("positions");
    let before = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");

    // A record two past the head of the execution log.
    let rows = petri_rows(&scenario.pool, scenario.run_id).await;
    let last = rows
        .iter()
        .filter(|row| row.0 == "execution 0")
        .max_by_key(|row| row.1)
        .expect("the execution log has records");
    let mut torn: serde_json::Value = serde_json::from_str(&last.3).expect("the record is JSON");
    torn["seq"] = serde_json::json!(last.1 + 2);
    insert_petri_row(
        &scenario.pool,
        scenario.run_id,
        &(last.0.clone(), last.1 + 2, last.2, torn.to_string()),
    )
    .await;

    let held = projector
        .project_run(scenario.run_id)
        .await
        .expect("the pass over the torn log still commits its health");
    assert!(!held.health.complete, "the torn log is incomplete");
    assert!(
        !held.health.incomplete.is_empty(),
        "the reason is reported: {:?}",
        held.health
    );
    let (positions_after, stream_after) =
        projector::stored_positions(&scenario.pool, scenario.run_id)
            .await
            .expect("reads")
            .expect("positions");
    assert_eq!(
        positions_after, positions,
        "the view did not advance past the tear"
    );
    assert_eq!(stream_after, stream_seq);
    let after = projector::stored_projection(&scenario.pool, scenario.run_id)
        .await
        .expect("reads")
        .expect("stored");
    assert_eq!(
        json(&before),
        json(&after),
        "the projection stands where it was"
    );
}

/// The run's records the projection reads (the coordinator log and the
/// execution logs; the sandbox ledger has no events) in the order they
/// were recorded: by `recorded_at`, the coordinator log first on a tie,
/// each log's own order kept. The ledger's records are copied to `staged`
/// first, since they are no part of any batch.
async fn in_recorded_order<'a>(
    rows: &'a [(String, i64, i64, String)],
    staged: &DbPool,
    run_id: RunId,
) -> Vec<&'a (String, i64, i64, String)> {
    let (ledger, projected): (Vec<_>, Vec<_>) = rows.iter().partition(|row| row.0 == "resources");
    for row in ledger {
        insert_petri_row(staged, run_id, row).await;
    }
    let mut ordered = projected;
    ordered.sort_by_key(|row| (row.2, row.0 != "coordinator", row.0.clone(), row.1));
    ordered
}

/// One committed pass over the run, run again while a platform record
/// contends it.
async fn committed_pass(projector: &Projector, run_id: RunId) -> projector::PassReport {
    loop {
        let report = projector
            .project_run(run_id)
            .await
            .expect("the pass commits");
        if !report.contended {
            return report;
        }
    }
}

/// The records land in batches and a pass follows each: every pass feeds
/// only its batch through Petri's derivation, never the run so far, and
/// the view the batches build is the rebuild. The finished run's cache is
/// dropped, and a pass over it is skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pass_over_a_live_run_costs_its_new_records_not_the_run() {
    const BATCH: usize = 7;
    if host_plugin().is_none() {
        return;
    }
    let scenario = parallel_scenario().await;
    run_unobserved(&scenario).await;
    let rows = petri_rows(&scenario.pool, scenario.run_id).await;
    let staged = copy_run_without_records(&scenario.pool, scenario.run_id).await;
    let ordered = in_recorded_order(&rows, &staged, scenario.run_id).await;
    assert!(
        ordered.len() > 4 * BATCH,
        "enough records for several batches: {}",
        ordered.len()
    );
    let projector = Projector::new(staged.clone(), staged.clone());
    let mut replayed = Vec::new();
    for batch in ordered.chunks(BATCH) {
        for row in batch {
            insert_petri_row(&staged, scenario.run_id, row).await;
        }
        let report = committed_pass(&projector, scenario.run_id).await;
        assert!(!report.skipped, "a batch is folded: {report:?}");
        assert!(
            report.replayed_records <= batch.len(),
            "pass {}: {} records replayed for a batch of {}",
            replayed.len(),
            report.replayed_records,
            batch.len()
        );
        replayed.push(report.replayed_records);
    }
    assert_eq!(
        replayed.iter().sum::<usize>(),
        ordered.len(),
        "every record was fed once: {replayed:?}"
    );
    assert_view_equals_rebuild(&staged, scenario.run_id).await;
    assert!(
        !petri_support::cache_held(&projector, scenario.run_id),
        "a finished run's cache is dropped"
    );
    let again = committed_pass(&projector, scenario.run_id).await;
    assert!(again.skipped, "nothing is left to fold: {again:?}");
    assert!(again.health.complete, "{:?}", again.health.incomplete);
}

/// The cache is dropped with the process and after the idle period, and
/// rebuilt by one full replay: the first pass over new records after
/// either feeds the run so far through Petri's derivation, the next only
/// its new records. Nothing is checkpointed for it, and the view it
/// continues is the rebuild.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_and_the_idle_period_drop_the_cache_and_one_full_replay_rebuilds_it() {
    const BATCH: usize = 5;
    if host_plugin().is_none() {
        return;
    }
    let scenario = parallel_scenario().await;
    run_unobserved(&scenario).await;
    let rows = petri_rows(&scenario.pool, scenario.run_id).await;
    let staged = copy_run_without_records(&scenario.pool, scenario.run_id).await;
    let ordered = in_recorded_order(&rows, &staged, scenario.run_id).await;
    let half = ordered.len() / 2;
    assert!(half > 3 * BATCH, "enough records: {}", ordered.len());
    let mut fed = 0;
    let mut feed = |count: usize| {
        let rows: Vec<_> = ordered[fed..(fed + count).min(ordered.len())].to_vec();
        fed += rows.len();
        rows
    };

    for row in feed(half) {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    let before = Projector::new(staged.clone(), staged.clone());
    let first = committed_pass(&before, scenario.run_id).await;
    assert_eq!(
        first.replayed_records, half,
        "the first pass replays the run so far"
    );
    assert!(!first.health.complete, "the run has not finished");
    assert!(
        petri_support::cache_held(&before, scenario.run_id),
        "a live run's cache is kept"
    );
    drop(before);

    // A restarted server builds a new projector: no cache, and the next
    // pass replays the run whole once.
    let after = Projector::new(staged.clone(), staged.clone());
    assert!(
        !petri_support::cache_held(&after, scenario.run_id),
        "a restart holds no cache"
    );
    for row in feed(BATCH) {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    let rebuilt = committed_pass(&after, scenario.run_id).await;
    assert_eq!(
        rebuilt.replayed_records,
        half + BATCH,
        "the first pass after a restart replays the run so far"
    );
    assert!(petri_support::cache_held(&after, scenario.run_id));
    for row in feed(BATCH) {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    let live = committed_pass(&after, scenario.run_id).await;
    assert_eq!(
        live.replayed_records, BATCH,
        "the next pass replays its batch"
    );

    // The idle period passes: the cache is dropped, and rebuilt the same way.
    assert_eq!(
        petri_support::drop_idle_caches(&after, Duration::ZERO),
        1,
        "the run's cache was idle"
    );
    assert!(!petri_support::cache_held(&after, scenario.run_id));
    for row in feed(BATCH) {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    let idle = committed_pass(&after, scenario.run_id).await;
    assert_eq!(idle.replayed_records, half + 3 * BATCH);
    let rest = feed(ordered.len());
    let rest_len = rest.len();
    for row in rest {
        insert_petri_row(&staged, scenario.run_id, row).await;
    }
    let last = committed_pass(&after, scenario.run_id).await;
    assert_eq!(last.replayed_records, rest_len);
    assert!(last.health.complete, "{:?}", last.health.incomplete);
    assert_view_equals_rebuild(&staged, scenario.run_id).await;
}

/// A gate scenario runs through the engine assembly with the interview
/// adapter, as a Fabro run does, over a store that signals the projector.
struct GateRun {
    scenario:  Scenario,
    markers:   PathBuf,
    projector: Arc<Projector>,
    workflow:  String,
    _root:     tempfile::TempDir,
}

async fn gate_run(gate_attrs: &str) -> GateRun {
    gate_run_of(|markers| gate_workflow(markers, gate_attrs)).await
}

async fn gate_run_of(workflow: impl FnOnce(&Path) -> String) -> GateRun {
    let root = tempfile::tempdir().expect("a marker dir");
    let markers = root.path().join("markers");
    fs::create_dir_all(&markers)
        .await
        .expect("the marker dir creates");
    let workflow = workflow(&markers);
    let scenario = scenario(
        "gate",
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        false,
    )
    .await;
    let projector = Projector::new(scenario.pool.clone(), scenario.pool.clone());
    projector.signal(scenario.run_id);
    GateRun {
        scenario,
        markers,
        projector,
        workflow,
        _root: root,
    }
}

impl GateRun {
    /// Run the gate to completion through the adapter, under `approval`,
    /// with nobody answering.
    async fn run(&self, approval: Approval) {
        let runtime = RuntimeSpec::default();
        let graphs = support::admit(
            &[
                ("workflow.fabro", &self.workflow),
                ("workflow.toml", SETTINGS),
            ],
            Launch::default(),
            &runtime,
        );
        let interviewer = FabroInterviewer::new(Arc::new(ControlInterviewer::new()), approval);
        let store = self
            .projector
            .observe_store(Arc::new(SqliteRunStore::new(self.scenario.pool.clone())));
        let request = support::run_request(
            &self.scenario.run_id.to_string(),
            &self.scenario.run_dir,
            graphs,
            store,
            runtime,
            interviewer,
        );
        let outcome = engine::run(request).await.expect("the run ends");
        assert_eq!(outcome.status, EngineRunStatus::Success, "{outcome:?}");
        self.projector.settle(self.scenario.run_id).await;
    }

    /// The stored projection once a question is pending in it.
    async fn pending(&self) -> fabro_types::RunProjection {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let stored = projector::stored_projection(&self.scenario.pool, self.scenario.run_id)
                .await
                .expect("the stored projection reads");
            if let Some(stored) = stored.filter(|stored| !stored.pending_interviews.is_empty()) {
                return stored;
            }
            assert!(
                Instant::now() < deadline,
                "the question never showed as pending"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn stored(&self) -> fabro_types::RunProjection {
        projector::stored_projection(&self.scenario.pool, self.scenario.run_id)
            .await
            .expect("the stored projection reads")
            .expect("the run has a stored projection")
    }
}

/// A question Petri expires: while the gate waits, the projection shows
/// the question pending under Petri's id and the firing's label with the
/// run blocked; once `question_expired` lands and the gate takes its
/// default, the question is gone, the run runs on to success, and the view
/// rebuilds the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_question_is_pending_while_the_gate_waits_and_closes_on_the_expiry() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Arc::new(gate_run(r#", timeout="1500ms", human.default_choice="no""#).await);
    let running = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move { gate.run(Approval::Prompt).await })
    };
    let pending = {
        let pool = gate.scenario.pool.clone();
        let run_id = gate.scenario.run_id;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let stored = projector::stored_projection(&pool, run_id)
                .await
                .expect("the stored projection reads");
            if let Some(stored) = stored.filter(|stored| !stored.pending_interviews.is_empty()) {
                break stored;
            }
            assert!(
                Instant::now() < deadline,
                "the question never showed as pending"
            );
            sleep(Duration::from_millis(10)).await;
        }
    };
    let (id, record) = pending
        .pending_interviews
        .iter()
        .next()
        .expect("one pending question");
    assert!(id.starts_with("gate#"), "Petri's id: {id}");
    assert_eq!(&record.question.id, id);
    assert_eq!(record.question.stage, "gate@1");
    assert_eq!(record.question.text, "Go?");
    assert_eq!(
        record
            .question
            .options
            .iter()
            .map(|option| option.key.as_str())
            .collect::<Vec<_>>(),
        vec!["Y", "N"]
    );
    assert_eq!(record.question.timeout_seconds, Some(1.5));
    assert!(
        matches!(pending.status, RunStatus::Blocked { .. }),
        "{:?}",
        pending.status
    );

    running.await.expect("the run task ends");

    assert!(
        gate.markers.join("no").exists() && !gate.markers.join("yes").exists(),
        "the default ran"
    );
    let stored = gate.stored().await;
    assert!(
        stored.pending_interviews.is_empty(),
        "the expired question is no longer pending: {:?}",
        stored.pending_interviews
    );
    assert!(
        matches!(stored.status, RunStatus::Succeeded { .. }),
        "{:?}",
        stored.status
    );
    let gate_stage = stored
        .stage(&StageId::new("gate", 1))
        .expect("the gate is a stage");
    assert_eq!(gate_stage.state, StageState::Succeeded);
    assert_view_equals_rebuild(&gate.scenario.pool, gate.scenario.run_id).await;
}

/// A gate's choices carry what choosing them means and a sample of what
/// they would do, and the question carries the previous stage's response
/// as its context: the pending question in the projection shows all
/// three, as Petri's question record carries them, and leaves them absent
/// on a choice that has none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_question_carries_its_choice_descriptions_previews_and_context() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Arc::new(gate_run_of(described_gate_workflow).await);
    let running = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move { gate.run(Approval::Prompt).await })
    };
    let pending = gate.pending().await;
    let (_, record) = pending
        .pending_interviews
        .iter()
        .next()
        .expect("one pending question");
    let question = &record.question;
    assert_eq!(question.stage, "gate@1");
    assert_eq!(question.text, "Deploy?");
    assert_eq!(
        question.context_display.as_deref(),
        Some("Ship the fix in one commit."),
        "the context is the previous stage's response"
    );
    assert_eq!(question.options.len(), 2, "{:?}", question.options);
    assert_eq!(question.options[0].key, "Y");
    assert_eq!(
        question.options[0].description.as_deref(),
        Some("Merge and deploy to production")
    );
    assert_eq!(
        question.options[0].preview.as_deref(),
        Some("deploy --prod")
    );
    assert_eq!(question.options[1].key, "N");
    assert_eq!(question.options[1].description, None);
    assert_eq!(question.options[1].preview, None);
    assert!(question.review_target.is_none());

    running.await.expect("the run task ends");
    assert!(gate.markers.join("no").exists(), "the default ran");
    assert_view_equals_rebuild(&gate.scenario.pool, gate.scenario.run_id).await;
}

/// An auto-approved run answers its gate at once: the delivered answer
/// closes the question in the projection, the affirmative branch runs, and
/// the view rebuilds the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_auto_approved_answer_closes_the_question_in_the_projection() {
    if host_plugin().is_none() {
        return;
    }
    let gate = gate_run("").await;
    gate.run(Approval::Auto).await;

    assert!(
        gate.markers.join("yes").exists() && !gate.markers.join("no").exists(),
        "the yes branch ran"
    );
    let stored = gate.stored().await;
    assert!(
        stored.pending_interviews.is_empty(),
        "the answered question is no longer pending: {:?}",
        stored.pending_interviews
    );
    assert!(
        matches!(stored.status, RunStatus::Succeeded { .. }),
        "{:?}",
        stored.status
    );
    let gate_stage = stored
        .stage(&StageId::new("gate", 1))
        .expect("the gate is a stage");
    assert_eq!(gate_stage.state, StageState::Succeeded);
    let states = stage_states(&gate.scenario.pool, gate.scenario.run_id).await;
    assert!(
        states
            .iter()
            .any(|(label, state)| label == "yes@1" && *state == StageState::Succeeded),
        "{states:?}"
    );
    assert_view_equals_rebuild(&gate.scenario.pool, gate.scenario.run_id).await;
}
