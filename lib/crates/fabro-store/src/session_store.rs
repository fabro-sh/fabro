use std::fs as std_fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::{
    SessionEventEnvelope, SessionId, SessionRecord, SessionStatus, SessionSummary, TurnId,
    TurnRecord, TurnStatus,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct SessionStore {
    root:       PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl SessionStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root:       root.into(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn create_session(&self, mut record: SessionRecord) -> Result<SessionRecord> {
        let _guard = self.write_lock.lock().await;
        let dir = self.session_dir(record.id);
        match fs::try_exists(&dir).await {
            Ok(true) => return Err(Error::SessionAlreadyExists(record.id.to_string())),
            Ok(false) => {}
            Err(err) => return Err(err.into()),
        }
        fs::create_dir_all(dir.join("turns")).await?;
        record.deleted_at = None;
        write_json(&self.session_path(record.id), &record).await?;
        Ok(record)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut summaries = Vec::new();
        let mut entries = match fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let Ok(session_id) = name.parse::<SessionId>() else {
                continue;
            };
            let Some(record) = self.get_session_including_deleted(session_id).await? else {
                continue;
            };
            if record.deleted_at.is_none() {
                summaries.push(SessionSummary::from(&record));
            }
        }
        summaries.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(summaries)
    }

    pub async fn get_session(&self, id: SessionId) -> Result<Option<SessionRecord>> {
        let Some(record) = self.get_session_including_deleted(id).await? else {
            return Ok(None);
        };
        Ok(record.deleted_at.is_none().then_some(record))
    }

    async fn get_session_including_deleted(&self, id: SessionId) -> Result<Option<SessionRecord>> {
        read_optional_json(&self.session_path(id)).await
    }

    pub async fn update_session(&self, record: SessionRecord) -> Result<SessionRecord> {
        let _guard = self.write_lock.lock().await;
        if self
            .get_session_including_deleted(record.id)
            .await?
            .is_none()
        {
            return Err(Error::SessionNotFound(record.id.to_string()));
        }
        write_json(&self.session_path(record.id), &record).await?;
        Ok(record)
    }

    pub async fn delete_session(&self, id: SessionId) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let Some(mut record) = self.get_session_including_deleted(id).await? else {
            return Ok(());
        };
        let now = Utc::now();
        record.status = SessionStatus::Deleted;
        record.updated_at = now;
        record.deleted_at = Some(now);
        write_json(&self.session_path(id), &record).await
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "Server startup recovery runs from synchronous AppState construction before routes are served."
    )]
    pub fn recover_stale_running_state(&self, recovered_at: DateTime<Utc>) -> Result<()> {
        let entries = match std_fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let Ok(session_id) = name.parse::<SessionId>() else {
                continue;
            };
            let session_path = self.session_path(session_id);
            let mut session: SessionRecord = match read_json_sync(&session_path) {
                Ok(session) => session,
                Err(Error::Io(err)) if err.kind() == ErrorKind::NotFound => continue,
                Err(Error::Serde(_)) => continue,
                Err(err) => return Err(err),
            };
            if session.deleted_at.is_some() {
                continue;
            }
            if session.status == SessionStatus::Running {
                session.status = SessionStatus::Idle;
                session.updated_at = recovered_at;
                write_json_sync(&session_path, &session)?;
            }
            self.recover_stale_turns(session_id, recovered_at)?;
        }
        Ok(())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "Server startup recovery runs from synchronous AppState construction before routes are served."
    )]
    fn recover_stale_turns(
        &self,
        session_id: SessionId,
        recovered_at: DateTime<Utc>,
    ) -> Result<()> {
        let entries = match std_fs::read_dir(self.turns_dir(session_id)) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stem.parse::<TurnId>().is_err() {
                continue;
            }
            let mut turn: TurnRecord = match read_json_sync(&path) {
                Ok(turn) => turn,
                Err(Error::Io(err)) if err.kind() == ErrorKind::NotFound => continue,
                Err(Error::Serde(_)) => continue,
                Err(err) => return Err(err),
            };
            if turn.status != TurnStatus::Running {
                continue;
            }
            turn.status = TurnStatus::Interrupted;
            turn.updated_at = recovered_at;
            turn.completed_at = Some(recovered_at);
            turn.error = Some("Server restarted before the turn completed.".to_string());
            write_json_sync(&path, &turn)?;
        }
        Ok(())
    }

    pub async fn append_turn(&self, record: TurnRecord) -> Result<TurnRecord> {
        let _guard = self.write_lock.lock().await;
        if self.get_session(record.session_id).await?.is_none() {
            return Err(Error::SessionNotFound(record.session_id.to_string()));
        }
        fs::create_dir_all(self.turns_dir(record.session_id)).await?;
        write_json(&self.turn_path(record.session_id, record.id), &record).await?;
        Ok(record)
    }

    pub async fn update_turn(&self, record: TurnRecord) -> Result<TurnRecord> {
        let _guard = self.write_lock.lock().await;
        if self.get_session(record.session_id).await?.is_none() {
            return Err(Error::SessionNotFound(record.session_id.to_string()));
        }
        let path = self.turn_path(record.session_id, record.id);
        match fs::try_exists(&path).await {
            Ok(true) => {}
            Ok(false) => return Err(Error::SessionNotFound(record.id.to_string())),
            Err(err) => return Err(err.into()),
        }
        write_json(&path, &record).await?;
        Ok(record)
    }

    pub async fn get_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<TurnRecord>> {
        read_optional_json(&self.turn_path(session_id, turn_id)).await
    }

    pub async fn list_turns(&self, session_id: SessionId) -> Result<Vec<TurnRecord>> {
        let mut turns: Vec<TurnRecord> = Vec::new();
        let mut entries = match fs::read_dir(self.turns_dir(session_id)).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stem.parse::<TurnId>().is_err() {
                continue;
            }
            turns.push(read_json(&path).await?);
        }
        turns.sort_by_key(|turn| turn.created_at);
        Ok(turns)
    }

    pub async fn append_event(
        &self,
        mut event: SessionEventEnvelope,
    ) -> Result<SessionEventEnvelope> {
        let _guard = self.write_lock.lock().await;
        if self.get_session(event.session_id).await?.is_none() {
            return Err(Error::SessionNotFound(event.session_id.to_string()));
        }
        let path = self.events_path(event.session_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        event.seq = next_event_seq(&path).await?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        file.write_all(&bytes).await?;
        file.flush().await?;
        Ok(event)
    }

    pub async fn list_events(
        &self,
        session_id: SessionId,
        since_seq: Option<u32>,
    ) -> Result<Vec<SessionEventEnvelope>> {
        let start = since_seq.unwrap_or(1);
        let path = self.events_path(session_id);
        let contents = match fs::read_to_string(path).await {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let mut events = Vec::new();
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let event: SessionEventEnvelope = serde_json::from_str(line)?;
            if event.seq >= start {
                events.push(event);
            }
        }
        events.sort_by_key(|event| event.seq);
        Ok(events)
    }

    fn session_dir(&self, id: SessionId) -> PathBuf {
        self.root.join(id.to_string())
    }

    fn session_path(&self, id: SessionId) -> PathBuf {
        self.session_dir(id).join("session.json")
    }

    fn turns_dir(&self, session_id: SessionId) -> PathBuf {
        self.session_dir(session_id).join("turns")
    }

    fn turn_path(&self, session_id: SessionId, turn_id: TurnId) -> PathBuf {
        self.turns_dir(session_id).join(format!("{turn_id}.json"))
    }

    fn events_path(&self, session_id: SessionId) -> PathBuf {
        self.session_dir(session_id).join("events.jsonl")
    }
}

async fn next_event_seq(path: &Path) -> Result<u32> {
    let contents = match fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(1),
        Err(err) => return Err(err.into()),
    };
    let mut max_seq = 0;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let event: SessionEventEnvelope = serde_json::from_str(line)?;
        max_seq = max_seq.max(event.seq);
    }
    Ok(max_seq.saturating_add(1).max(1))
}

async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes).await?;
    Ok(())
}

async fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn read_optional_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Used only by synchronous server startup recovery before routes are served."
)]
fn write_json_sync<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std_fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    std_fs::write(path, bytes)?;
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "Used only by synchronous server startup recovery before routes are served."
)]
fn read_json_sync<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std_fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}
