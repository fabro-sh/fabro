#![allow(clippy::absolute_paths)]

mod artifacts;
mod exec;
mod lifecycle;
mod recovery;
mod server_lifecycle;
mod smoke;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cmd::support::RunProjection;
use fabro_config::Storage;
use fabro_server::bind::Bind;
pub(super) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/workflow/fixtures")
        .join(name)
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn infer_run_id(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .expect("run dir should contain resolvable run id")
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

pub(super) fn run_state(run_dir: &Path) -> RunProjection {
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    let run_id = infer_run_id(run_dir);
    block_on(get_server_json_for_storage(
        storage_dir,
        &format!("/api/v1/runs/{run_id}/state"),
    ))
}

pub(super) fn timeout_for(sandbox: &str) -> Duration {
    match sandbox {
        "daytona" => Duration::from_secs(600),
        _ => Duration::from_secs(180),
    }
}
