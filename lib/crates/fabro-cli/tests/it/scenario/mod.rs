mod exec;
mod lifecycle;
mod recovery;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fabro_store::{RunSnapshot, RunStoreHandle, SlateStore};
use fabro_types::RunId;
use object_store::local::LocalFileSystem;
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

fn run_store(run_dir: &Path) -> RunStoreHandle {
    let runs_dir = run_dir.parent().expect("run dir should have parent");
    let storage_dir = runs_dir.parent().expect("runs dir should have parent");
    let run_id: RunId = std::fs::read_to_string(run_dir.join("id.txt"))
        .ok()
        .map(|id| id.trim().to_string())
        .or_else(|| {
            run_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        })
        .expect("run dir should contain resolvable run id")
        .parse()
        .expect("run id should parse");
    let object_store = Arc::new(
        LocalFileSystem::new_with_prefix(storage_dir.join("store"))
            .expect("test store path should be accessible"),
    );
    let store = Arc::new(SlateStore::new(object_store, "", Duration::from_millis(1)));
    block_on(store.open_run_reader(&run_id)).expect("run store should exist")
}

pub(super) fn run_snapshot(run_dir: &Path) -> RunSnapshot {
    let store = run_store(run_dir);
    block_on(store.state())
        .ok()
        .and_then(|state| state.to_snapshot())
        .expect("run store snapshot should exist")
}

pub(super) fn timeout_for(sandbox: &str) -> Duration {
    match sandbox {
        "daytona" => Duration::from_secs(600),
        _ => Duration::from_secs(180),
    }
}
