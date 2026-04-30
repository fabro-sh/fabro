use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fabro_config::RunScratch;
use fabro_store::stage_storage_segment;
use fabro_types::{CommandOutputStream, StageId, format_blob_ref};
use serde_json::Value;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::runtime_store::RunStoreHandle;

#[derive(Debug, Clone)]
pub struct FinalizedCommandLogs {
    pub stdout_ref:   String,
    pub stderr_ref:   String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_text:  String,
    pub stderr_text:  String,
}

pub struct CommandLogRecorder {
    stdout:       Mutex<File>,
    stderr:       Mutex<File>,
    stdout_bytes: AtomicU64,
    stderr_bytes: AtomicU64,
    stdout_path:  PathBuf,
    stderr_path:  PathBuf,
}

impl CommandLogRecorder {
    pub async fn create(run_dir: &Path, stage_id: &StageId) -> Result<Arc<Self>> {
        let stdout_path = command_log_path(run_dir, stage_id, CommandOutputStream::Stdout);
        let stderr_path = command_log_path(run_dir, stage_id, CommandOutputStream::Stderr);
        if let Some(parent) = stdout_path.parent() {
            fs::create_dir_all(parent).await.map_err(|err| {
                Error::Io(format!(
                    "creating command log directory {}: {err}",
                    parent.display()
                ))
            })?;
        }
        let stdout = open_truncated(&stdout_path).await?;
        let stderr = open_truncated(&stderr_path).await?;
        Ok(Arc::new(Self {
            stdout: Mutex::new(stdout),
            stderr: Mutex::new(stderr),
            stdout_bytes: AtomicU64::new(0),
            stderr_bytes: AtomicU64::new(0),
            stdout_path,
            stderr_path,
        }))
    }

    pub async fn append(&self, stream: CommandOutputStream, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = match stream {
            CommandOutputStream::Stdout => self.stdout.lock().await,
            CommandOutputStream::Stderr => self.stderr.lock().await,
        };
        file.write_all(bytes)
            .await
            .map_err(|err| Error::Io(format!("writing command {stream} log failed: {err}")))?;
        file.flush()
            .await
            .map_err(|err| Error::Io(format!("flushing command {stream} log failed: {err}")))?;
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        match stream {
            CommandOutputStream::Stdout => {
                self.stdout_bytes.fetch_add(len, Ordering::Relaxed);
            }
            CommandOutputStream::Stderr => {
                self.stderr_bytes.fetch_add(len, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    pub async fn finalize(&self, run_store: &RunStoreHandle) -> Result<FinalizedCommandLogs> {
        self.flush_all().await?;
        let stdout_text = read_lossy_text(&self.stdout_path).await?;
        let stderr_text = read_lossy_text(&self.stderr_path).await?;
        let stdout_bytes = self.stdout_bytes();
        let stderr_bytes = self.stderr_bytes();
        let stdout_ref = write_json_string_blob(run_store, &stdout_text).await?;
        let stderr_ref = write_json_string_blob(run_store, &stderr_text).await?;
        Ok(FinalizedCommandLogs {
            stdout_ref,
            stderr_ref,
            stdout_bytes,
            stderr_bytes,
            stdout_text,
            stderr_text,
        })
    }

    pub fn stdout_bytes(&self) -> u64 {
        self.stdout_bytes.load(Ordering::Relaxed)
    }

    pub fn stderr_bytes(&self) -> u64 {
        self.stderr_bytes.load(Ordering::Relaxed)
    }

    async fn flush_all(&self) -> Result<()> {
        self.stdout
            .lock()
            .await
            .flush()
            .await
            .map_err(|err| Error::Io(format!("flushing stdout command log failed: {err}")))?;
        self.stderr
            .lock()
            .await
            .flush()
            .await
            .map_err(|err| Error::Io(format!("flushing stderr command log failed: {err}")))?;
        Ok(())
    }
}

pub fn command_log_path(
    run_dir: &Path,
    stage_id: &StageId,
    stream: CommandOutputStream,
) -> PathBuf {
    RunScratch::new(run_dir)
        .runtime_dir()
        .join("stages")
        .join(stage_storage_segment(stage_id))
        .join(stream.command_log_relative_path())
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
        .map_err(|err| Error::engine(format!("command log blob read failed: {err}")))?
        .ok_or_else(|| Error::engine(format!("command log blob missing: {blob_id}")))?;
    let text = serde_json::from_slice::<String>(&bytes)
        .map_err(|err| Error::engine(format!("command log blob was not a JSON string: {err}")))?;
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

async fn read_lossy_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path)
        .await
        .map_err(|err| Error::Io(format!("reading command log {}: {err}", path.display())))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn write_json_string_blob(run_store: &RunStoreHandle, text: &str) -> Result<String> {
    let value = Value::String(text.to_string());
    let bytes = serde_json::to_vec(&value)
        .map_err(|err| Error::engine(format!("command log JSON serialization failed: {err}")))?;
    let blob_id = run_store
        .write_blob(&bytes)
        .await
        .map_err(|err| Error::engine(format!("command log blob write failed: {err}")))?;
    Ok(format_blob_ref(&blob_id))
}
