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
use std::time::Duration;

use fabro_test::TestContext;
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
    let cp = read_json(&run_dir.join("checkpoint.json"));
    cp["completed_nodes"]
        .as_array()
        .expect("completed_nodes should be an array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
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
