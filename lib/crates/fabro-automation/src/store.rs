use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

use crate::{
    Automation, AutomationDraft, AutomationId, AutomationReplace, AutomationRevision,
    AutomationStoreError,
};

#[derive(Debug)]
pub struct AutomationStore {
    dir:         PathBuf,
    mutations:   Mutex<()>,
    automations: RwLock<HashMap<AutomationId, Automation>>,
}

impl AutomationStore {
    pub async fn load(dir: impl Into<PathBuf>) -> Result<Self, AutomationStoreError> {
        let dir = dir.into();
        let automations = load_automations(&dir).await?;
        Ok(Self {
            dir,
            mutations: Mutex::new(()),
            automations: RwLock::new(automations),
        })
    }

    pub async fn list(&self) -> Vec<Automation> {
        let automations = self.automations.read().await;
        let mut values = automations.values().cloned().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        values
    }

    pub async fn get(&self, id: &AutomationId) -> Option<Automation> {
        self.automations.read().await.get(id).cloned()
    }

    pub async fn create(&self, draft: AutomationDraft) -> Result<Automation, AutomationStoreError> {
        let (id, replace) = draft.into();
        let (automation, bytes) = Automation::from_replace(id.clone(), replace)?;
        let _mutation = self.mutations.lock().await;
        if self.automations.read().await.contains_key(&id) {
            return Err(AutomationStoreError::AlreadyExists { id });
        }

        let path = automation_path(&self.dir, &id);
        write_new(&self.dir, &path, &bytes)
            .await
            .map_err(|err| create_error_for(id.clone(), err))?;

        let mut automations = self.automations.write().await;
        automations.insert(id, automation.clone());
        Ok(automation)
    }

    pub async fn replace(
        &self,
        id: &AutomationId,
        expected: &AutomationRevision,
        draft: AutomationReplace,
    ) -> Result<Automation, AutomationStoreError> {
        let (automation, bytes) = Automation::from_replace(id.clone(), draft)?;
        let _mutation = self.mutations.lock().await;
        {
            let automations = self.automations.read().await;
            let current = automations
                .get(id)
                .ok_or_else(|| AutomationStoreError::NotFound { id: id.clone() })?;
            if &current.revision != expected {
                return Err(AutomationStoreError::StaleRevision {
                    id:       id.clone(),
                    expected: expected.clone(),
                    actual:   current.revision.clone(),
                });
            }
        }

        write_atomic(&self.dir, &automation_path(&self.dir, id), &bytes).await?;
        let mut automations = self.automations.write().await;
        automations.insert(id.clone(), automation.clone());
        Ok(automation)
    }

    pub async fn delete(
        &self,
        id: &AutomationId,
        expected: &AutomationRevision,
    ) -> Result<(), AutomationStoreError> {
        let _mutation = self.mutations.lock().await;
        {
            let automations = self.automations.read().await;
            let current = automations
                .get(id)
                .ok_or_else(|| AutomationStoreError::NotFound { id: id.clone() })?;
            if &current.revision != expected {
                return Err(AutomationStoreError::StaleRevision {
                    id:       id.clone(),
                    expected: expected.clone(),
                    actual:   current.revision.clone(),
                });
            }
        }

        let path = automation_path(&self.dir, id);
        fs::remove_file(&path)
            .await
            .map_err(|err| AutomationStoreError::io(path, err))?;
        let mut automations = self.automations.write().await;
        automations.remove(id);
        Ok(())
    }
}

async fn load_automations(
    dir: &Path,
) -> Result<HashMap<AutomationId, Automation>, AutomationStoreError> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(AutomationStoreError::io(dir, err)),
    };

    let mut automations = HashMap::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| AutomationStoreError::io(dir, err))?
    {
        let path = entry.path();
        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            Err(err) => {
                warn_load_failure(&path, &AutomationStoreError::io(path.clone(), err));
                continue;
            }
        };
        if !file_type.is_file() || !is_toml_file(&path) {
            continue;
        }

        match load_automation_file(&path).await {
            Ok(automation) => {
                automations.insert(automation.id.clone(), automation);
            }
            Err(err) => warn_load_failure(&path, &err),
        }
    }
    Ok(automations)
}

async fn load_automation_file(path: &Path) -> Result<Automation, AutomationStoreError> {
    let id = id_from_path(path)?;
    let bytes = fs::read(path)
        .await
        .map_err(|err| AutomationStoreError::io(path, err))?;
    Automation::from_persisted_path(id, &bytes, path)
}

fn id_from_path(path: &Path) -> Result<AutomationId, AutomationStoreError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AutomationStoreError::InvalidFilename {
            path:   path.to_path_buf(),
            reason: "filename is not valid UTF-8".to_string(),
        })?;
    AutomationId::new(stem).map_err(|source| AutomationStoreError::InvalidFilename {
        path:   path.to_path_buf(),
        reason: source.to_string(),
    })
}

fn is_toml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "toml")
}

async fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), AutomationStoreError> {
    fs::create_dir_all(dir)
        .await
        .map_err(|err| AutomationStoreError::io(dir, err))?;
    let temp_path = temp_path_for(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|err| AutomationStoreError::io(&temp_path, err))?;

    if let Err(err) = file.write_all(bytes).await {
        cleanup_temp(&temp_path).await;
        return Err(AutomationStoreError::io(&temp_path, err));
    }
    if let Err(err) = file.sync_all().await {
        cleanup_temp(&temp_path).await;
        return Err(AutomationStoreError::io(&temp_path, err));
    }
    drop(file);

    if let Err(err) = fs::rename(&temp_path, path).await {
        cleanup_temp(&temp_path).await;
        return Err(AutomationStoreError::io(path, err));
    }

    Ok(())
}

async fn write_new(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), AutomationStoreError> {
    fs::create_dir_all(dir)
        .await
        .map_err(|err| AutomationStoreError::io(dir, err))?;
    let temp_path = temp_path_for(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|err| AutomationStoreError::io(&temp_path, err))?;

    if let Err(err) = file.write_all(bytes).await {
        cleanup_temp(&temp_path).await;
        return Err(AutomationStoreError::io(&temp_path, err));
    }
    if let Err(err) = file.sync_all().await {
        cleanup_temp(&temp_path).await;
        return Err(AutomationStoreError::io(&temp_path, err));
    }
    drop(file);

    if let Err(err) = fs::hard_link(&temp_path, path).await {
        cleanup_temp(&temp_path).await;
        return Err(AutomationStoreError::io(path, err));
    }
    cleanup_temp(&temp_path).await;
    Ok(())
}

async fn cleanup_temp(path: &Path) {
    let _ = fs::remove_file(path).await;
}

fn create_error_for(id: AutomationId, err: AutomationStoreError) -> AutomationStoreError {
    match err {
        AutomationStoreError::Io { source, .. } if source.kind() == ErrorKind::AlreadyExists => {
            AutomationStoreError::AlreadyExists { id }
        }
        err => err,
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("automation.toml");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), now))
}

fn automation_path(dir: &Path, id: &AutomationId) -> PathBuf {
    dir.join(format!("{id}.toml"))
}

fn warn_load_failure(path: &Path, err: &AutomationStoreError) {
    warn!(
        path = %path.display(),
        failure_kind = err.kind(),
        error = %err,
        "Skipping automation file"
    );
}

#[cfg(test)]
mod tests {
    use tokio::fs;

    use crate::{
        ApiTrigger, AutomationDraft, AutomationId, AutomationReplace, AutomationStore,
        AutomationStoreError, AutomationTarget, AutomationTrigger, AutomationTriggerId,
        ScheduleTrigger,
    };

    fn target() -> AutomationTarget {
        AutomationTarget {
            repository:   "fabro-sh/fabro".to_string(),
            ref_selector: "main".to_string(),
            workflow:     "release".to_string(),
        }
    }

    fn draft(id: &str, name: &str) -> AutomationDraft {
        AutomationDraft {
            id:          AutomationId::new(id).unwrap(),
            name:        name.to_string(),
            description: None,
            enabled:     true,
            target:      target(),
            triggers:    vec![
                AutomationTrigger::Api(ApiTrigger {
                    id:      AutomationTriggerId::new("manual").unwrap(),
                    enabled: true,
                }),
                AutomationTrigger::Schedule(ScheduleTrigger {
                    id:         AutomationTriggerId::new("nightly").unwrap(),
                    enabled:    true,
                    expression: "0 0 * * *".to_string(),
                }),
            ],
        }
    }

    fn replacement(name: &str) -> AutomationReplace {
        AutomationReplace {
            name:        name.to_string(),
            description: Some("updated".to_string()),
            enabled:     false,
            target:      target(),
            triggers:    vec![AutomationTrigger::Api(ApiTrigger {
                id:      AutomationTriggerId::new("manual").unwrap(),
                enabled: false,
            })],
        }
    }

    #[tokio::test]
    async fn missing_directory_loads_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = AutomationStore::load(dir.path().join("automations"))
            .await
            .unwrap();

        assert!(store.list().await.is_empty());
    }

    #[tokio::test]
    async fn load_skips_invalid_files_and_keeps_valid_automations() {
        let dir = tempfile::tempdir().unwrap();
        let automation_dir = dir.path().join("automations");
        fs::create_dir_all(&automation_dir).await.unwrap();
        fs::write(automation_dir.join("notes.txt"), "ignore")
            .await
            .unwrap();
        fs::write(automation_dir.join("bad name.toml"), "name = \"Bad\"")
            .await
            .unwrap();
        fs::write(automation_dir.join("broken.toml"), "not valid toml =")
            .await
            .unwrap();
        fs::write(
            automation_dir.join("empty-name.toml"),
            r#"
name = " "

[target]
repository = "fabro-sh/fabro"
workflow = "release"
"#,
        )
        .await
        .unwrap();
        fs::write(
            automation_dir.join("valid.toml"),
            r#"
name = "Valid"

[target]
repository = "fabro-sh/fabro"
ref = "main"
workflow = "release"
"#,
        )
        .await
        .unwrap();

        let store = AutomationStore::load(&automation_dir).await.unwrap();
        let automations = store.list().await;

        assert_eq!(automations.len(), 1);
        assert_eq!(automations[0].id.as_str(), "valid");
        assert_eq!(automations[0].name, "Valid");
    }

    #[tokio::test]
    async fn create_does_not_overwrite_existing_malformed_file() {
        let dir = tempfile::tempdir().unwrap();
        let automation_dir = dir.path().join("automations");
        fs::create_dir_all(&automation_dir).await.unwrap();
        let path = automation_dir.join("nightly.toml");
        fs::write(&path, "not valid toml =").await.unwrap();

        let store = AutomationStore::load(&automation_dir).await.unwrap();
        let result = store.create(draft("nightly", "Nightly")).await;

        assert!(matches!(
            result,
            Err(AutomationStoreError::AlreadyExists { id }) if id.as_str() == "nightly"
        ));
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "not valid toml =");
    }

    #[tokio::test]
    async fn create_replace_and_delete_round_trip_files_and_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let automation_dir = dir.path().join("automations");
        let store = AutomationStore::load(&automation_dir).await.unwrap();

        let created = store.create(draft("nightly", "Nightly")).await.unwrap();
        let path = automation_dir.join("nightly.toml");
        let persisted = fs::read_to_string(&path).await.unwrap();
        assert!(persisted.contains("name = \"Nightly\""));
        assert!(!top_level_lines(&persisted).any(|line| line.starts_with("id = ")));
        assert!(!top_level_lines(&persisted).any(|line| line.starts_with("revision = ")));
        assert_eq!(
            created.revision,
            crate::AutomationRevision::from_bytes(persisted.as_bytes())
        );
        assert!(store.create(draft("nightly", "Duplicate")).await.is_err());

        let stale = crate::AutomationRevision::from_bytes(b"stale");
        assert!(
            store
                .replace(&created.id, &stale, replacement("Updated"))
                .await
                .is_err()
        );

        let replaced = store
            .replace(&created.id, &created.revision, replacement("Updated"))
            .await
            .unwrap();
        assert_ne!(replaced.revision, created.revision);
        assert_eq!(
            store.get(&created.id).await.unwrap().revision,
            replaced.revision
        );

        store.delete(&created.id, &replaced.revision).await.unwrap();
        assert!(store.get(&created.id).await.is_none());
        assert!(!path.exists());
    }

    fn top_level_lines(toml: &str) -> impl Iterator<Item = &str> {
        toml.lines().take_while(|line| !line.starts_with('['))
    }
}
