use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use fabro_types::settings::run::McpServerSettings;
use fabro_types::{
    McpServerDefinition, McpServerDraft, McpServerId, McpServerReplace, McpServerRevision,
    McpServerView,
};
use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

use crate::error::McpServerStoreError;
use crate::model;

/// Durable per-file TOML store for server-managed MCP server definitions.
///
/// Concrete by design (no trait): a future FS→SQL move is a one-time migration,
/// not a runtime backend choice. The method surface keeps callers storage-
/// agnostic; only [`McpServerStore::load`] knows about the filesystem.
#[derive(Debug)]
pub struct McpServerStore {
    dir:       PathBuf,
    mutations: Mutex<()>,
    defs:      RwLock<HashMap<McpServerId, McpServerDefinition>>,
}

impl McpServerStore {
    /// Synchronously load every persisted definition in `dir`. Returns an error
    /// if any file fails to parse or validate; the caller decides startup
    /// failure policy. Synchronous because it runs once at construction time
    /// (typically during server startup) and is invoked from non-async code.
    pub fn load(dir: impl Into<PathBuf>) -> Result<Self, McpServerStoreError> {
        let dir = dir.into();
        let defs = load_definitions(&dir)?;
        Ok(Self {
            dir,
            mutations: Mutex::new(()),
            defs: RwLock::new(defs),
        })
    }

    fn read_defs(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<McpServerId, McpServerDefinition>> {
        self.defs.read().expect("mcp server store lock poisoned")
    }

    fn write_defs(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<McpServerId, McpServerDefinition>> {
        self.defs.write().expect("mcp server store lock poisoned")
    }

    pub fn list(&self) -> Vec<McpServerDefinition> {
        let defs = self.read_defs();
        let mut values = defs.values().cloned().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        values
    }

    pub fn list_views(&self) -> Vec<McpServerView> {
        let defs = self.read_defs();
        let mut values = defs.values().map(McpServerView::from).collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        values
    }

    pub fn catalog_settings(&self) -> HashMap<String, McpServerSettings> {
        let defs = self.read_defs();
        defs.iter()
            .map(|(id, definition)| (id.to_string(), server_settings_from_definition(definition)))
            .collect()
    }

    /// Sorted ids only, without cloning the (potentially sensitive) env/header
    /// maps carried by full definitions. Used by missing-reference errors to
    /// list available ids cheaply.
    pub fn ids(&self) -> Vec<McpServerId> {
        let defs = self.read_defs();
        let mut ids = defs.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn get(&self, id: &McpServerId) -> Option<McpServerDefinition> {
        self.read_defs().get(id).cloned()
    }

    pub async fn create(
        &self,
        draft: McpServerDraft,
    ) -> Result<McpServerDefinition, McpServerStoreError> {
        let (id, replace) = draft.into();
        let _mutation = self.mutations.lock().await;
        if self.read_defs().contains_key(&id) {
            return Err(McpServerStoreError::AlreadyExists { id });
        }
        let (definition, bytes) = model::definition_from_replace(id.clone(), replace)?;

        let path = definition_path(&self.dir, &id);
        write_new(&self.dir, &path, &bytes)
            .await
            .map_err(|err| create_error_for(id.clone(), err))?;

        let mut defs = self.write_defs();
        defs.insert(id, definition.clone());
        Ok(definition)
    }

    pub async fn replace(
        &self,
        id: &McpServerId,
        expected: &McpServerRevision,
        replace: McpServerReplace,
    ) -> Result<McpServerDefinition, McpServerStoreError> {
        let _mutation = self.mutations.lock().await;
        {
            let defs = self.read_defs();
            check_revision(&defs, id, expected)?;
        }
        let (definition, bytes) = model::definition_from_replace(id.clone(), replace)?;

        write_atomic(&self.dir, &definition_path(&self.dir, id), &bytes).await?;
        let mut defs = self.write_defs();
        defs.insert(id.clone(), definition.clone());
        Ok(definition)
    }

    pub async fn delete(
        &self,
        id: &McpServerId,
        expected: &McpServerRevision,
    ) -> Result<(), McpServerStoreError> {
        let _mutation = self.mutations.lock().await;
        {
            let defs = self.read_defs();
            check_revision(&defs, id, expected)?;
        }

        let path = definition_path(&self.dir, id);
        fs::remove_file(&path)
            .await
            .map_err(|err| McpServerStoreError::io(path, err))?;
        let mut defs = self.write_defs();
        defs.remove(id);
        Ok(())
    }
}

fn check_revision(
    defs: &HashMap<McpServerId, McpServerDefinition>,
    id: &McpServerId,
    expected: &McpServerRevision,
) -> Result<(), McpServerStoreError> {
    let current = defs
        .get(id)
        .ok_or_else(|| McpServerStoreError::NotFound { id: id.clone() })?;
    if &current.revision != expected {
        return Err(McpServerStoreError::StaleRevision {
            id:       id.clone(),
            expected: expected.clone(),
            actual:   current.revision.clone(),
        });
    }
    Ok(())
}

fn server_settings_from_definition(definition: &McpServerDefinition) -> McpServerSettings {
    McpServerSettings {
        name:                 definition.id.to_string(),
        transport:            definition.transport.clone(),
        current_dir:          None,
        clear_env:            false,
        startup_timeout_secs: definition.startup_timeout_secs,
        tool_timeout_secs:    definition.tool_timeout_secs,
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "MCP server directory scan runs once at startup, before the runtime needs to make progress; std::fs avoids needing a Tokio runtime for the caller."
)]
fn load_definitions(
    dir: &Path,
) -> Result<HashMap<McpServerId, McpServerDefinition>, McpServerStoreError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(McpServerStoreError::io(dir, err)),
    };

    let mut defs = HashMap::new();
    for entry in entries {
        let entry = entry.map_err(|err| McpServerStoreError::io(dir, err))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| McpServerStoreError::io(&path, err))?;
        if !file_type.is_file() || !is_toml_file(&path) {
            continue;
        }
        let definition = load_definition_file(&path)?;
        defs.insert(definition.id.clone(), definition);
    }
    Ok(defs)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Sync sibling of `load_definitions`; only invoked from the synchronous startup load path."
)]
fn load_definition_file(path: &Path) -> Result<McpServerDefinition, McpServerStoreError> {
    let id = id_from_path(path)?;
    let bytes = std::fs::read(path).map_err(|err| McpServerStoreError::io(path, err))?;
    model::definition_from_persisted_path(id, &bytes, path)
}

fn id_from_path(path: &Path) -> Result<McpServerId, McpServerStoreError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| McpServerStoreError::InvalidFilename {
            path:   path.to_path_buf(),
            reason: "filename is not valid UTF-8".to_string(),
        })?;
    McpServerId::new(stem).map_err(|source| McpServerStoreError::InvalidFilename {
        path:   path.to_path_buf(),
        reason: source.to_string(),
    })
}

fn is_toml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "toml")
}

async fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), McpServerStoreError> {
    let temp_path = write_temp_file(dir, path, bytes).await?;
    if let Err(err) = fs::rename(&temp_path, path).await {
        cleanup_temp(&temp_path).await;
        return Err(McpServerStoreError::io(path, err));
    }

    Ok(())
}

async fn write_new(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), McpServerStoreError> {
    let temp_path = write_temp_file(dir, path, bytes).await?;
    if let Err(err) = fs::hard_link(&temp_path, path).await {
        cleanup_temp(&temp_path).await;
        return Err(McpServerStoreError::io(path, err));
    }
    cleanup_temp(&temp_path).await;
    Ok(())
}

async fn write_temp_file(
    dir: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf, McpServerStoreError> {
    fs::create_dir_all(dir)
        .await
        .map_err(|err| McpServerStoreError::io(dir, err))?;
    let temp_path = temp_path_for(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|err| McpServerStoreError::io(&temp_path, err))?;

    if let Err(err) = file.write_all(bytes).await {
        cleanup_temp(&temp_path).await;
        return Err(McpServerStoreError::io(&temp_path, err));
    }
    if let Err(err) = file.sync_all().await {
        cleanup_temp(&temp_path).await;
        return Err(McpServerStoreError::io(&temp_path, err));
    }
    drop(file);

    Ok(temp_path)
}

async fn cleanup_temp(path: &Path) {
    let _ = fs::remove_file(path).await;
}

fn create_error_for(id: McpServerId, err: McpServerStoreError) -> McpServerStoreError {
    match err {
        McpServerStoreError::Io { source, .. } if source.kind() == ErrorKind::AlreadyExists => {
            McpServerStoreError::AlreadyExists { id }
        }
        err => err,
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mcp-server.toml");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), now))
}

fn definition_path(dir: &Path, id: &McpServerId) -> PathBuf {
    dir.join(format!("{id}.toml"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::settings::McpTransport;
    use fabro_types::settings::run::McpHttpProtocol;
    use fabro_types::{McpServerDraft, McpServerId, McpServerReplace, McpServerRevision};
    use tokio::fs;

    use crate::error::McpServerStoreError;
    use crate::store::McpServerStore;

    fn http_transport(url: &str) -> McpTransport {
        McpTransport::Http {
            protocol: McpHttpProtocol::default(),
            url:      url.to_string(),
            headers:  HashMap::new(),
        }
    }

    fn draft(id: &str, display_name: &str) -> McpServerDraft {
        McpServerDraft {
            id:                   McpServerId::new(id).unwrap(),
            display_name:         display_name.to_string(),
            description:          None,
            transport:            http_transport("https://example.com/mcp"),
            startup_timeout_secs: 10,
            tool_timeout_secs:    60,
        }
    }

    fn replacement(display_name: &str) -> McpServerReplace {
        McpServerReplace {
            display_name:         display_name.to_string(),
            description:          Some("updated".to_string()),
            transport:            http_transport("https://example.com/mcp/v2"),
            startup_timeout_secs: 15,
            tool_timeout_secs:    90,
        }
    }

    #[tokio::test]
    async fn missing_directory_loads_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = McpServerStore::load(dir.path().join("mcps")).unwrap();

        assert!(store.list().is_empty());
        assert!(store.ids().is_empty());
    }

    #[tokio::test]
    async fn load_ignores_non_toml_files_and_keeps_valid_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_dir = dir.path().join("mcps");
        fs::create_dir_all(&mcp_dir).await.unwrap();
        fs::write(mcp_dir.join("notes.txt"), "ignore")
            .await
            .unwrap();
        fs::write(
            mcp_dir.join("sentry.toml"),
            r#"
display_name = "Sentry"
startup_timeout_secs = 10
tool_timeout_secs = 60

[transport]
type = "http"
url = "https://sentry.example.com/mcp"

[transport.headers]
"#,
        )
        .await
        .unwrap();

        let store = McpServerStore::load(&mcp_dir).unwrap();
        let defs = store.list();

        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].id.as_str(), "sentry");
        assert_eq!(defs[0].display_name, "Sentry");
    }

    #[tokio::test]
    async fn load_fails_on_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_dir = dir.path().join("mcps");
        fs::create_dir_all(&mcp_dir).await.unwrap();
        fs::write(mcp_dir.join("broken.toml"), "not valid toml =")
            .await
            .unwrap();

        let err = McpServerStore::load(&mcp_dir).unwrap_err();
        assert!(matches!(err, McpServerStoreError::Parse { .. }));
    }

    #[tokio::test]
    async fn load_fails_on_invalid_filename_id() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_dir = dir.path().join("mcps");
        fs::create_dir_all(&mcp_dir).await.unwrap();
        fs::write(mcp_dir.join("Bad Name.toml"), "display_name = \"Bad\"")
            .await
            .unwrap();

        let err = McpServerStore::load(&mcp_dir).unwrap_err();
        assert!(matches!(err, McpServerStoreError::InvalidFilename { .. }));
    }

    #[tokio::test]
    async fn create_get_list_replace_and_delete_round_trip_files_and_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_dir = dir.path().join("mcps");
        let store = McpServerStore::load(&mcp_dir).unwrap();

        let created = store.create(draft("sentry", "Sentry")).await.unwrap();
        let path = mcp_dir.join("sentry.toml");
        let persisted = fs::read_to_string(&path).await.unwrap();
        assert!(persisted.contains("display_name = \"Sentry\""));
        assert!(!top_level_lines(&persisted).any(|line| line.starts_with("id = ")));
        assert!(!top_level_lines(&persisted).any(|line| line.starts_with("revision = ")));
        assert_eq!(
            created.revision,
            McpServerRevision::from_bytes(persisted.as_bytes())
        );

        assert_eq!(store.get(&created.id).unwrap(), created);
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(store.ids(), vec![created.id.clone()]);

        let replaced = store
            .replace(&created.id, &created.revision, replacement("Sentry v2"))
            .await
            .unwrap();
        assert_ne!(replaced.revision, created.revision);
        assert_eq!(replaced.display_name, "Sentry v2");
        assert_eq!(store.get(&created.id).unwrap().revision, replaced.revision);

        store.delete(&created.id, &replaced.revision).await.unwrap();
        assert!(store.get(&created.id).is_none());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn replace_with_stale_revision_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = McpServerStore::load(dir.path().join("mcps")).unwrap();
        let created = store.create(draft("sentry", "Sentry")).await.unwrap();

        let stale = McpServerRevision::from_bytes(b"stale");
        let err = store
            .replace(&created.id, &stale, replacement("Updated"))
            .await
            .unwrap_err();
        assert!(matches!(err, McpServerStoreError::StaleRevision { .. }));

        // The on-disk and in-memory definition is unchanged after a rejected replace.
        assert_eq!(store.get(&created.id).unwrap(), created);
    }

    #[tokio::test]
    async fn duplicate_create_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = McpServerStore::load(dir.path().join("mcps")).unwrap();
        store.create(draft("sentry", "Sentry")).await.unwrap();

        let err = store
            .create(draft("sentry", "Duplicate"))
            .await
            .unwrap_err();
        assert!(matches!(err, McpServerStoreError::AlreadyExists { .. }));
    }

    fn top_level_lines(toml: &str) -> impl Iterator<Item = &str> {
        toml.lines().take_while(|line| !line.starts_with('['))
    }
}
