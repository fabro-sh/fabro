#![allow(clippy::absolute_paths)]

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

use crate::cmd::support::RunProjection;
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

/// Find the single run directory for this test context.
pub(super) fn find_run_dir(context: &TestContext) -> PathBuf {
    let runs_base = context.storage_dir.join("scratch");
    let runs: Vec<RunSummaryRecord> = block_on(get_server_json_for_storage(
        &context.storage_dir,
        "/api/v1/runs",
    ));
    let entries: Vec<_> = runs
        .into_iter()
        .filter(|run| {
            run.labels
                .get("fabro_test_case")
                .is_some_and(|value| value == context.test_case_id())
        })
        .filter_map(|run| find_run_dir_for_id(&context.storage_dir, &run.run_id))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one run directory for fabro_test_case={} under {}",
        context.test_case_id(),
        runs_base.display()
    );
    entries[0].clone()
}

fn infer_run_id(run_dir: &Path) -> String {
    if let Ok(id) = std::fs::read_to_string(run_dir.join("id.txt")) {
        return id.trim().to_string();
    }
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .filter(|value| !value.is_empty())
        .expect("run directory name should contain run id suffix")
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct RunSummaryRecord {
    run_id: String,
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn server_http_client(storage_dir: &Path) -> reqwest::Client {
    reqwest::ClientBuilder::new()
        .unix_socket(storage_dir.join("fabro.sock"))
        .no_proxy()
        .build()
        .expect("test HTTP client should build")
}

async fn get_server_json_for_storage<T: serde::de::DeserializeOwned>(
    storage_dir: &Path,
    path: &str,
) -> T {
    let response = server_http_client(storage_dir)
        .get(format!("http://fabro{path}"))
        .send()
        .await
        .expect("server request should succeed");
    assert!(
        response.status().is_success(),
        "server request failed for {path}: {}",
        response.status()
    );
    response
        .json::<T>()
        .await
        .expect("server response should parse")
}

fn find_run_dir_for_id(storage_dir: &Path, run_id: &str) -> Option<PathBuf> {
    let runs_dir = storage_dir.join("scratch");
    let entries = std::fs::read_dir(&runs_dir).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(run_id))
        })
}

fn run_state(run_dir: &Path) -> RunProjection {
    let run_id = infer_run_id(run_dir);
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    block_on(get_server_json_for_storage(
        storage_dir,
        &format!("/api/v1/runs/{run_id}/state"),
    ))
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
