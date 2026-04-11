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

use fabro_config::Storage;
use fabro_server::bind::Bind;
use fabro_store::EventEnvelope;
use fabro_test::TestContext;
use serde_json::Value;

use crate::cmd::support::RunProjection;

pub(super) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/workflow/fixtures")
        .join(name)
}

pub(super) fn read_conclusion(run_dir: &Path) -> Value {
    serde_json::to_value(
        run_state(run_dir)
            .conclusion
            .expect("run store conclusion should exist"),
    )
    .expect("conclusion should serialize")
}

pub(super) fn read_run_record(run_dir: &Path) -> Value {
    serde_json::to_value(
        run_state(run_dir)
            .run
            .expect("run store run record should exist"),
    )
    .expect("run record should serialize")
}

pub(super) fn completed_nodes(run_dir: &Path) -> Vec<String> {
    let cp = run_state(run_dir)
        .checkpoint
        .expect("run store checkpoint should exist");
    cp.completed_nodes
}

pub(super) fn has_event(run_dir: &Path, event_name: &str) -> bool {
    run_events(run_dir).into_iter().any(|event| {
        event
            .payload
            .as_value()
            .get("event")
            .and_then(Value::as_str)
            == Some(event_name)
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
    context.single_run_dir()
}

pub(super) fn run_id_for(run_dir: &Path) -> String {
    infer_run_id(run_dir)
}

fn infer_run_id(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .filter(|value| !value.is_empty())
        .expect("run directory name should contain run id suffix")
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[derive(Debug, serde::Deserialize)]
struct TestServerRecord {
    bind: Bind,
}

fn server_endpoint(storage_dir: &Path) -> (reqwest::Client, String) {
    let record_path = Storage::new(storage_dir).server_state().record_path();
    let record: TestServerRecord = serde_json::from_str(
        &std::fs::read_to_string(record_path).expect("server record should exist"),
    )
    .expect("server record should parse");
    match record.bind {
        Bind::Unix(path) => (
            reqwest::ClientBuilder::new()
                .unix_socket(path)
                .no_proxy()
                .build()
                .expect("test Unix-socket HTTP client should build"),
            "http://fabro".to_string(),
        ),
        Bind::Tcp(addr) => (
            reqwest::ClientBuilder::new()
                .no_proxy()
                .build()
                .expect("test TCP HTTP client should build"),
            format!("http://{addr}"),
        ),
    }
}

async fn get_server_json_for_storage<T: serde::de::DeserializeOwned>(
    storage_dir: &Path,
    path: &str,
) -> T {
    let (client, base_url) = server_endpoint(storage_dir);
    let response = client
        .get(format!("{base_url}{path}"))
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

fn run_state(run_dir: &Path) -> RunProjection {
    let run_id = infer_run_id(run_dir);
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    block_on(get_server_json_for_storage(
        storage_dir,
        &format!("/api/v1/runs/{run_id}/state"),
    ))
}

fn run_events(run_dir: &Path) -> Vec<EventEnvelope> {
    let run_id = infer_run_id(run_dir);
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    let response: serde_json::Value = block_on(get_server_json_for_storage(
        storage_dir,
        &format!("/api/v1/runs/{run_id}/events"),
    ));
    crate::support::parse_event_envelopes(&response)
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
