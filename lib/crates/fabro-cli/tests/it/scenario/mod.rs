#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]

mod archive;
mod artifacts;
mod auth;
mod exec;
mod lifecycle;
mod recovery;
mod server_lifecycle;
mod smoke;

use std::path::{Path, PathBuf};
use std::time::Duration;

use fabro_test::expect_reqwest_status;

use crate::cmd::support::{RunProjection, server_endpoint};
pub(super) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/workflow/fixtures")
        .join(name)
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should build")
        .block_on(future)
}

fn infer_run_id(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .expect("run dir should contain resolvable run id")
}

async fn get_server_json_for_storage<T: serde::de::DeserializeOwned>(
    storage_dir: &Path,
    path: &str,
) -> T {
    let (client, base_url) = server_endpoint(storage_dir).expect("server endpoint should exist");
    let response = client
        .get(format!("{base_url}{path}"))
        .send()
        .await
        .expect("server request should succeed");
    let response =
        expect_reqwest_status(response, fabro_http::StatusCode::OK, format!("GET {path}")).await;
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
        "daytona" => Duration::from_mins(10),
        _ => Duration::from_mins(3),
    }
}
