mod exec;
mod lifecycle;
mod recovery;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fabro_store::{RunSnapshot, RunStore, SlateStore, Store};
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

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn run_store(run_dir: &Path) -> Option<Arc<dyn RunStore>> {
    let runs_dir = run_dir.parent()?;
    let storage_dir = runs_dir.parent()?;
    let run_id: RunId = std::fs::read_to_string(run_dir.join("id.txt"))
        .ok()
        .map(|id| id.trim().to_string())
        .or_else(|| {
            run_dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        })?
        .parse()
        .ok()?;
    let object_store = Arc::new(LocalFileSystem::new_with_prefix(storage_dir.join("store")).ok()?);
    let store = Arc::new(SlateStore::new(object_store, "", Duration::from_millis(5)));
    block_on(store.open_run_reader(&run_id)).ok().flatten()
}

pub(super) fn run_snapshot(run_dir: &Path) -> RunSnapshot {
    run_store(run_dir)
        .and_then(|store| block_on(store.get_snapshot()).ok())
        .flatten()
        .expect("run store snapshot should exist")
}

pub(super) fn timeout_for(sandbox: &str) -> Duration {
    match sandbox {
        "daytona" => Duration::from_secs(600),
        _ => Duration::from_secs(180),
    }
}
