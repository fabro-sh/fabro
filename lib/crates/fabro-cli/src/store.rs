use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fabro_store::{SlateRunStore, SlateStore};
use fabro_types::RunId;
use object_store::local::LocalFileSystem;

pub(crate) fn build_store(storage_dir: &Path) -> Result<Arc<SlateStore>> {
    let store_path = storage_dir.join("store");
    std::fs::create_dir_all(&store_path)?;
    let object_store = Arc::new(LocalFileSystem::new_with_prefix(&store_path)?);
    Ok(Arc::new(SlateStore::new(
        object_store,
        "",
        Duration::from_millis(1),
    )))
}

pub(crate) async fn open_run_reader(storage_dir: &Path, run_id: &RunId) -> Result<SlateRunStore> {
    build_store(storage_dir)?
        .open_run_reader(run_id)
        .await
        .map_err(Into::into)
}
