mod exec;
mod lifecycle;
mod recovery;
mod server_lifecycle;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cmd::support::RunProjection;
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
    std::fs::read_to_string(run_dir.join("id.txt"))
        .ok()
        .map(|id| id.trim().to_string())
        .or_else(|| {
            run_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        })
        .expect("run dir should contain resolvable run id")
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
