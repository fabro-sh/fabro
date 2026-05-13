use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_config::RunScratch;
use fabro_store::stage_storage_segment;
use fabro_types::{StageId, format_blob_ref};
use serde_json::Value;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::runtime_store::RunStoreHandle;

#[derive(Debug, Clone)]
pub struct FinalizedCommandLogs {
    pub output_ref:   String,
    pub output_bytes: u64,
    pub output_text:  String,
}

pub struct CommandLogRecorder {
    output:      Mutex<File>,
    output_path: PathBuf,
}

impl CommandLogRecorder {
    pub async fn create(run_dir: &Path, stage_id: &StageId) -> Result<Arc<Self>> {
        let output_path = command_log_path(run_dir, stage_id);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).await.map_err(|err| {
                Error::Io(format!(
                    "creating command log directory {}: {err}",
                    parent.display()
                ))
            })?;
        }
        let output = open_truncated(&output_path).await?;
        Ok(Arc::new(Self {
            output: Mutex::new(output),
            output_path,
        }))
    }

    pub async fn append(&self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = self.output.lock().await;
        file.write_all(bytes)
            .await
            .map_err(|err| Error::Io(format!("writing command output log failed: {err}")))?;
        Ok(())
    }

    pub async fn finalize(&self, run_store: &RunStoreHandle) -> Result<FinalizedCommandLogs> {
        self.flush_all().await?;
        let (output_text, output_bytes) = read_lossy_text(&self.output_path).await?;
        let output_ref = write_json_string_blob(run_store, &output_text).await?;
        Ok(FinalizedCommandLogs {
            output_ref,
            output_bytes,
            output_text,
        })
    }

    pub async fn discard(self: Arc<Self>) -> Result<()> {
        self.flush_all().await?;
        let output_path = self.output_path.clone();
        drop(self);
        remove_if_exists(&output_path).await
    }

    async fn flush_all(&self) -> Result<()> {
        self.output
            .lock()
            .await
            .flush()
            .await
            .map_err(|err| Error::Io(format!("flushing command output log failed: {err}")))?;
        Ok(())
    }
}

pub fn command_log_path(run_dir: &Path, stage_id: &StageId) -> PathBuf {
    RunScratch::new(run_dir)
        .runtime_dir()
        .join("stages")
        .join(stage_storage_segment(stage_id))
        .join("output.log")
}

pub async fn read_log_slice(
    path: &Path,
    offset: u64,
    limit: u64,
) -> std::io::Result<(Vec<u8>, u64)> {
    let mut file = fs::File::open(path).await?;
    let total = file.metadata().await?.len();
    let start = offset.min(total);
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let take = limit.min(total.saturating_sub(start));
    let mut buf = vec![0; usize::try_from(take).unwrap_or(usize::MAX)];
    file.read_exact(&mut buf).await?;
    Ok((buf, total))
}

pub async fn read_json_string_blob(
    run_store: &RunStoreHandle,
    blob_ref: &str,
) -> Result<Option<String>> {
    let Some(blob_id) = fabro_types::parse_blob_ref(blob_ref) else {
        return Ok(None);
    };
    let bytes = run_store
        .read_blob(&blob_id)
        .await
        .map_err(|err| Error::engine_with_anyhow("command log blob read failed", err))?
        .ok_or_else(|| Error::engine(format!("command log blob missing: {blob_id}")))?;
    let text = serde_json::from_slice::<String>(&bytes)
        .map_err(|err| Error::engine_with_source("command log blob was not a JSON string", err))?;
    Ok(Some(text))
}

async fn open_truncated(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await
        .map_err(|err| Error::Io(format!("opening command log {}: {err}", path.display())))
}

async fn read_lossy_text(path: &Path) -> Result<(String, u64)> {
    let bytes = fs::read(path)
        .await
        .map_err(|err| Error::Io(format!("reading command log {}: {err}", path.display())))?;
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    Ok((String::from_utf8_lossy(&bytes).into_owned(), len))
}

async fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::Io(format!(
            "removing command log {}: {err}",
            path.display()
        ))),
    }
}

async fn write_json_string_blob(run_store: &RunStoreHandle, text: &str) -> Result<String> {
    let value = Value::String(text.to_string());
    let bytes = serde_json::to_vec(&value)
        .map_err(|err| Error::engine_with_source("command log JSON serialization failed", err))?;
    let blob_id = run_store
        .write_blob(&bytes)
        .await
        .map_err(|err| Error::engine_with_anyhow("command log blob write failed", err))?;
    Ok(format_blob_ref(&blob_id))
}
