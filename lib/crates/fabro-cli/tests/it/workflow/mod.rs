mod agent_linear;
mod command_agent_mixed;
mod command_pipeline;
mod conditional_branching;
mod dry_run_examples;
mod full_stack;
mod hooks;
mod human_gate;
mod real_cli;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fabro_store::{SlateRunStore, SlateStore};
use fabro_test::TestContext;
use fabro_types::RunId;
use object_store::local::LocalFileSystem;
use serde_json::Value;

pub(super) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/workflow/fixtures")
        .join(name)
}

pub(super) fn read_json(path: &Path) -> Value {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

pub(super) fn read_conclusion(run_dir: &Path) -> Value {
    read_json(&run_dir.join("conclusion.json"))
}

pub(super) fn completed_nodes(run_dir: &Path) -> Vec<String> {
    let cp = run_state(run_dir)
        .checkpoint
        .expect("run store checkpoint should exist");
    cp.completed_nodes
}

pub(super) fn has_event(run_dir: &Path, event_name: &str) -> bool {
    let path = run_dir.join("progress.jsonl");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read progress.jsonl: {e}"));
    content.lines().any(|line| {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            v["event"].as_str() == Some(event_name)
        } else {
            false
        }
    })
}

pub(super) fn store_dump_export(context: &TestContext, run_id: &str) -> PathBuf {
    let output_dir = context.temp_dir.join(format!("store-dump-{run_id}"));
    context
        .command()
        .args([
            "store",
            "dump",
            "--output",
            output_dir.to_str().unwrap(),
            run_id,
        ])
        .assert()
        .success();
    output_dir
}

/// Find the single run directory under `storage_dir/runs/`.
pub(super) fn find_run_dir(storage_dir: &Path) -> PathBuf {
    let runs_base = storage_dir.join("runs");
    let entries: Vec<_> = std::fs::read_dir(&runs_base)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", runs_base.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one run directory under {}",
        runs_base.display()
    );
    entries[0].path()
}

fn infer_run_id(run_dir: &Path) -> RunId {
    if let Ok(id) = std::fs::read_to_string(run_dir.join("id.txt")) {
        return id.trim().parse().expect("run id should parse");
    }
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .filter(|value| !value.is_empty())
        .expect("run directory name should contain run id suffix")
        .parse()
        .expect("run id should parse")
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn run_store(run_dir: &Path) -> SlateRunStore {
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    let object_store = Arc::new(
        LocalFileSystem::new_with_prefix(storage_dir.join("store"))
            .expect("test store path should be accessible"),
    );
    let store = Arc::new(SlateStore::new(object_store, "", Duration::from_millis(1)));
    block_on(store.open_run_reader(&infer_run_id(run_dir))).expect("run store should exist")
}

fn run_state(run_dir: &Path) -> fabro_store::RunProjection {
    block_on(run_store(run_dir).state()).expect("run store state should exist")
}

macro_rules! sandbox_tests {
    ($name:ident) => {
        sandbox_tests!($name, keys = []);
    };
    ($name:ident, keys = [$($key:expr),* $(,)?]) => {
        paste::paste! {
            #[fabro_macros::e2e_test($(live($key)),*)]
            fn [<local_ $name>]() {
                [<scenario_ $name>]("local");
            }

            #[fabro_macros::e2e_test(live("DAYTONA_API_KEY") $(, live($key))*)]
            fn [<daytona_ $name>]() {
                [<scenario_ $name>]("daytona");
            }
        }
    };
}
pub(super) use sandbox_tests;

pub(super) fn timeout_for(sandbox: &str) -> Duration {
    match sandbox {
        "daytona" => Duration::from_secs(600),
        _ => Duration::from_secs(180),
    }
}
