use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fabro_config::{EnvironmentLayer, MergeMap};
use fabro_types::settings::run::EnvironmentSettings;
use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

use crate::{
    Environment, EnvironmentDraft, EnvironmentId, EnvironmentRevision, EnvironmentStoreError,
};

const SEEDS: &[(&str, &str)] = &[
    ("default", DEFAULT_ENVIRONMENT_TOML),
    ("local", LOCAL_ENVIRONMENT_TOML),
    ("docker", DOCKER_ENVIRONMENT_TOML),
    ("daytona", DAYTONA_ENVIRONMENT_TOML),
];

/// Returns the built-in seeded environment catalog as a `MergeMap` of
/// `EnvironmentLayer`s. Useful for client-side manifest validation where no
/// live `EnvironmentStore` is available.
pub fn seeded_catalog_layer() -> MergeMap<EnvironmentLayer> {
    let mut catalog: HashMap<String, EnvironmentLayer> = HashMap::new();
    for (id, body) in SEEDS {
        let layer: EnvironmentLayer =
            toml::from_str(body).expect("built-in environment seed should parse");
        catalog.insert((*id).to_string(), layer);
    }
    MergeMap::from(catalog)
}

const DEFAULT_ENVIRONMENT_TOML: &str = r#"provider = "docker"

[image]
docker = "buildpack-deps:noble"

[resources]
cpu = 2
memory = "4GB"

[lifecycle]
preserve = false
stop_on_terminal = true
"#;

const LOCAL_ENVIRONMENT_TOML: &str = r#"provider = "local"
"#;

const DOCKER_ENVIRONMENT_TOML: &str = r#"provider = "docker"

[image]
docker = "buildpack-deps:noble"

[resources]
cpu = 2
memory = "4GB"

[lifecycle]
preserve = false
stop_on_terminal = true
"#;

const DAYTONA_ENVIRONMENT_TOML: &str = r#"provider = "daytona"
"#;

#[derive(Debug)]
pub struct EnvironmentStore {
    dir:              PathBuf,
    request_base_dir: PathBuf,
    mutations:        Mutex<()>,
    state:            std::sync::RwLock<CatalogState>,
}

#[derive(Debug, Clone)]
struct CatalogState {
    environments: HashMap<EnvironmentId, Environment>,
    catalog:      Arc<MergeMap<EnvironmentLayer>>,
}

impl CatalogState {
    fn new(environments: HashMap<EnvironmentId, Environment>) -> Self {
        let catalog = Arc::new(build_catalog_layer(&environments));
        Self {
            environments,
            catalog,
        }
    }

    fn refresh_catalog(&mut self) {
        self.catalog = Arc::new(build_catalog_layer(&self.environments));
    }
}

fn build_catalog_layer(
    environments: &HashMap<EnvironmentId, Environment>,
) -> MergeMap<EnvironmentLayer> {
    let catalog: HashMap<String, EnvironmentLayer> = environments
        .iter()
        .map(|(id, environment)| (id.to_string(), environment.to_layer()))
        .collect();
    MergeMap::from(catalog)
}

impl EnvironmentStore {
    /// Synchronously seed missing built-in environment files and load all
    /// persisted environments. The synchronous file access runs during server
    /// startup before request handling begins.
    pub fn load_or_seed(dir: impl Into<PathBuf>) -> Result<Self, EnvironmentStoreError> {
        let dir = dir.into();
        seed_missing_environments(&dir)?;
        let environments = load_environments(&dir)?;
        let request_base_dir = dir.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        Ok(Self {
            dir,
            request_base_dir,
            mutations: Mutex::new(()),
            state: std::sync::RwLock::new(CatalogState::new(environments)),
        })
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, CatalogState> {
        self.state.read().expect("environment store lock poisoned")
    }

    fn write_state(&self) -> std::sync::RwLockWriteGuard<'_, CatalogState> {
        self.state.write().expect("environment store lock poisoned")
    }

    pub fn list(&self) -> Vec<Environment> {
        let state = self.read_state();
        let mut values = state.environments.values().cloned().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        values
    }

    pub fn get(&self, id: &EnvironmentId) -> Option<Environment> {
        self.read_state().environments.get(id).cloned()
    }

    pub async fn create(
        &self,
        draft: EnvironmentDraft,
    ) -> Result<Environment, EnvironmentStoreError> {
        let EnvironmentDraft { id, settings } = draft;
        let (environment, bytes) =
            Environment::from_settings(id.clone(), settings, &self.request_base_dir).await?;
        let _mutation = self.mutations.lock().await;
        if self.read_state().environments.contains_key(&id) {
            return Err(EnvironmentStoreError::AlreadyExists { id });
        }

        let path = environment_path(&self.dir, &id);
        write_new(&self.dir, &path, &bytes)
            .await
            .map_err(|err| create_error_for(id.clone(), err))?;

        let mut state = self.write_state();
        state.environments.insert(id, environment.clone());
        state.refresh_catalog();
        Ok(environment)
    }

    pub async fn replace(
        &self,
        id: &EnvironmentId,
        expected: &EnvironmentRevision,
        settings: EnvironmentSettings,
    ) -> Result<Environment, EnvironmentStoreError> {
        let (environment, bytes) =
            Environment::from_settings(id.clone(), settings, &self.request_base_dir).await?;
        let _mutation = self.mutations.lock().await;
        check_revision(&self.read_state().environments, id, expected)?;

        write_atomic(&self.dir, &environment_path(&self.dir, id), &bytes).await?;
        let mut state = self.write_state();
        state.environments.insert(id.clone(), environment.clone());
        state.refresh_catalog();
        Ok(environment)
    }

    pub async fn delete(
        &self,
        id: &EnvironmentId,
        expected: &EnvironmentRevision,
    ) -> Result<(), EnvironmentStoreError> {
        if id.as_str() == "default" {
            return Err(EnvironmentStoreError::Protected { id: id.clone() });
        }

        let _mutation = self.mutations.lock().await;
        check_revision(&self.read_state().environments, id, expected)?;

        let path = environment_path(&self.dir, id);
        fs::remove_file(&path)
            .await
            .map_err(|err| EnvironmentStoreError::io(path, err))?;
        let mut state = self.write_state();
        state.environments.remove(id);
        state.refresh_catalog();
        Ok(())
    }

    pub fn catalog_layer(&self) -> Arc<MergeMap<EnvironmentLayer>> {
        Arc::clone(&self.read_state().catalog)
    }
}

fn check_revision(
    environments: &HashMap<EnvironmentId, Environment>,
    id: &EnvironmentId,
    expected: &EnvironmentRevision,
) -> Result<(), EnvironmentStoreError> {
    let current = environments
        .get(id)
        .ok_or_else(|| EnvironmentStoreError::NotFound { id: id.clone() })?;
    if &current.revision != expected {
        return Err(EnvironmentStoreError::StaleRevision {
            id:       id.clone(),
            expected: expected.clone(),
            actual:   current.revision.clone(),
        });
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Environment directory seeding runs synchronously during startup before request handling."
)]
fn seed_missing_environments(dir: &Path) -> Result<(), EnvironmentStoreError> {
    std::fs::create_dir_all(dir).map_err(|err| EnvironmentStoreError::io(dir, err))?;
    for (id, content) in SEEDS {
        let path = dir.join(format!("{id}.toml"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(content.as_bytes())
                    .map_err(|err| EnvironmentStoreError::io(&path, err))?;
                file.sync_all()
                    .map_err(|err| EnvironmentStoreError::io(&path, err))?;
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(EnvironmentStoreError::io(path, err)),
        }
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "Environment directory scan runs once at startup; std::fs avoids requiring a Tokio runtime for callers."
)]
fn load_environments(
    dir: &Path,
) -> Result<HashMap<EnvironmentId, Environment>, EnvironmentStoreError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(EnvironmentStoreError::io(dir, err)),
    };

    let mut environments = HashMap::new();
    for entry in entries {
        let entry = entry.map_err(|err| EnvironmentStoreError::io(dir, err))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| EnvironmentStoreError::io(&path, err))?;
        if !file_type.is_file() || !is_toml_file(&path) {
            continue;
        }
        let environment = load_environment_file(&path)?;
        environments.insert(environment.id.clone(), environment);
    }
    Ok(environments)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Sync sibling of `load_environments`; only invoked from the synchronous startup load path."
)]
fn load_environment_file(path: &Path) -> Result<Environment, EnvironmentStoreError> {
    let id = id_from_path(path)?;
    let bytes = std::fs::read(path).map_err(|err| EnvironmentStoreError::io(path, err))?;
    Environment::from_persisted_path(id, &bytes, path)
}

fn id_from_path(path: &Path) -> Result<EnvironmentId, EnvironmentStoreError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| EnvironmentStoreError::InvalidFilename {
            path:   path.to_path_buf(),
            reason: "filename is not valid UTF-8".to_string(),
        })?;
    EnvironmentId::new(stem).map_err(|source| EnvironmentStoreError::InvalidFilename {
        path:   path.to_path_buf(),
        reason: source.to_string(),
    })
}

fn is_toml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "toml")
}

async fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), EnvironmentStoreError> {
    fs::create_dir_all(dir)
        .await
        .map_err(|err| EnvironmentStoreError::io(dir, err))?;
    let temp_path = temp_path_for(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|err| EnvironmentStoreError::io(&temp_path, err))?;

    if let Err(err) = file.write_all(bytes).await {
        cleanup_temp(&temp_path).await;
        return Err(EnvironmentStoreError::io(&temp_path, err));
    }
    if let Err(err) = file.sync_all().await {
        cleanup_temp(&temp_path).await;
        return Err(EnvironmentStoreError::io(&temp_path, err));
    }
    drop(file);

    if let Err(err) = fs::rename(&temp_path, path).await {
        cleanup_temp(&temp_path).await;
        return Err(EnvironmentStoreError::io(path, err));
    }

    Ok(())
}

async fn write_new(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), EnvironmentStoreError> {
    fs::create_dir_all(dir)
        .await
        .map_err(|err| EnvironmentStoreError::io(dir, err))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|err| EnvironmentStoreError::io(path, err))?;
    file.write_all(bytes)
        .await
        .map_err(|err| EnvironmentStoreError::io(path, err))?;
    file.sync_all()
        .await
        .map_err(|err| EnvironmentStoreError::io(path, err))?;
    Ok(())
}

async fn cleanup_temp(path: &Path) {
    let _ = fs::remove_file(path).await;
}

fn create_error_for(id: EnvironmentId, err: EnvironmentStoreError) -> EnvironmentStoreError {
    match err {
        EnvironmentStoreError::Io { source, .. } if source.kind() == ErrorKind::AlreadyExists => {
            EnvironmentStoreError::AlreadyExists { id }
        }
        err => err,
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("environment.toml");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), now))
}

fn environment_path(dir: &Path, id: &EnvironmentId) -> PathBuf {
    dir.join(format!("{id}.toml"))
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "Unit tests for sync startup helpers use sync std::fs to set up fixtures."
)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::settings::InterpString;
    use fabro_types::settings::run::{
        DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
        EnvironmentNetworkMode, EnvironmentNetworkSettings, EnvironmentProvider,
        EnvironmentResourcesSettings, EnvironmentSettings,
    };
    use tokio::fs;

    use crate::{
        EnvironmentDraft, EnvironmentId, EnvironmentRevision, EnvironmentStore,
        EnvironmentStoreError,
    };

    fn settings(provider: EnvironmentProvider) -> EnvironmentSettings {
        EnvironmentSettings {
            provider,
            image: EnvironmentImageSettings::default(),
            resources: EnvironmentResourcesSettings::default(),
            network: EnvironmentNetworkSettings::default(),
            lifecycle: EnvironmentLifecycleSettings::default(),
            labels: HashMap::new(),
            volumes: Vec::new(),
            env: HashMap::new(),
        }
    }

    fn draft(id: &str, provider: EnvironmentProvider) -> EnvironmentDraft {
        EnvironmentDraft {
            id:       EnvironmentId::new(id).unwrap(),
            settings: settings(provider),
        }
    }

    #[test]
    fn seeded_catalog_layer_contains_built_ins() {
        let catalog = super::seeded_catalog_layer();
        let inner = catalog.into_inner();
        for id in ["default", "local", "docker", "daytona"] {
            assert!(inner.contains_key(id), "missing {id}");
        }
    }

    #[tokio::test]
    async fn absent_directory_loads_and_seeds_built_ins() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");

        let store = EnvironmentStore::load_or_seed(&environment_dir).unwrap();
        let environments = store.list();

        assert_eq!(
            environments
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["daytona", "default", "docker", "local"]
        );
        for id in ["default", "local", "docker", "daytona"] {
            assert!(environment_dir.join(format!("{id}.toml")).exists());
        }
    }

    #[tokio::test]
    async fn listing_is_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        fs::create_dir_all(&environment_dir).await.unwrap();
        fs::write(environment_dir.join("z.toml"), r#"provider = "local""#)
            .await
            .unwrap();
        fs::write(environment_dir.join("a.toml"), r#"provider = "local""#)
            .await
            .unwrap();

        let store = EnvironmentStore::load_or_seed(&environment_dir).unwrap();

        assert_eq!(
            store
                .list()
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "daytona", "default", "docker", "local", "z"]
        );
    }

    #[test]
    fn invalid_id_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(environment_dir.join("Bad.toml"), r#"provider = "local""#).unwrap();

        let err = EnvironmentStore::load_or_seed(&environment_dir).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::InvalidFilename { .. }));
    }

    #[test]
    fn invalid_provider_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(environment_dir.join("bad.toml"), r#"provider = "bogus""#).unwrap();

        let err = EnvironmentStore::load_or_seed(&environment_dir).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        assert!(err.to_string().contains("unknown environment provider"));
    }

    #[test]
    fn invalid_network_mode_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(
            environment_dir.join("bad.toml"),
            r#"
provider = "docker"

[network]
mode = "cidr_allow_list"
"#,
        )
        .unwrap();

        let err = EnvironmentStore::load_or_seed(&environment_dir).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        assert!(
            err.to_string()
                .contains("docker environments cannot enforce")
        );
    }

    #[test]
    fn missing_dockerfile_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(
            environment_dir.join("bad.toml"),
            r#"
provider = "docker"

[image.dockerfile]
path = "Dockerfile"
"#,
        )
        .unwrap();

        let err = EnvironmentStore::load_or_seed(&environment_dir).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        assert!(err.to_string().contains("Dockerfile"));
    }

    #[tokio::test]
    async fn create_conflict_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load_or_seed(dir.path().join("environments")).unwrap();

        let err = store
            .create(draft("local", EnvironmentProvider::Local))
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::AlreadyExists { .. }));
    }

    #[tokio::test]
    async fn create_invalid_settings_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load_or_seed(dir.path().join("environments")).unwrap();
        let mut settings = settings(EnvironmentProvider::Local);
        settings.network.mode = EnvironmentNetworkMode::Block;

        let err = store
            .create(EnvironmentDraft {
                id: EnvironmentId::new("invalid").unwrap(),
                settings,
            })
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        assert!(
            err.to_string()
                .contains("local environments cannot enforce")
        );
    }

    #[tokio::test]
    async fn replace_stale_revision_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load_or_seed(dir.path().join("environments")).unwrap();
        let current = store.get(&EnvironmentId::new("local").unwrap()).unwrap();
        let stale = EnvironmentRevision::from_bytes(b"stale");

        let err = store
            .replace(&current.id, &stale, settings(EnvironmentProvider::Docker))
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::StaleRevision { .. }));
    }

    #[tokio::test]
    async fn default_delete_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load_or_seed(dir.path().join("environments")).unwrap();
        let default = store.get(&EnvironmentId::new("default").unwrap()).unwrap();

        let err = store
            .delete(&default.id, &default.revision)
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Protected { .. }));
    }

    #[tokio::test]
    async fn delete_success_removes_file_and_memory_entry() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        let store = EnvironmentStore::load_or_seed(&environment_dir).unwrap();
        let created = store
            .create(draft("tmp", EnvironmentProvider::Local))
            .await
            .unwrap();

        store.delete(&created.id, &created.revision).await.unwrap();

        assert!(store.get(&created.id).is_none());
        assert!(!environment_dir.join("tmp.toml").exists());
    }

    #[tokio::test]
    async fn canonical_revision_changes_when_persisted_bytes_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load_or_seed(dir.path().join("environments")).unwrap();
        let created = store
            .create(draft("rev", EnvironmentProvider::Local))
            .await
            .unwrap();
        let mut next = settings(EnvironmentProvider::Local);
        next.env.insert(
            "TOKEN".to_string(),
            InterpString::parse("{{ env.TEST_TOKEN }}"),
        );

        let replaced = store
            .replace(&created.id, &created.revision, next)
            .await
            .unwrap();

        assert_ne!(created.revision, replaced.revision);
    }

    #[tokio::test]
    async fn api_dockerfile_path_is_resolved_relative_to_settings_dir_and_persisted_inline() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n")
            .await
            .unwrap();
        let store = EnvironmentStore::load_or_seed(dir.path().join("environments")).unwrap();
        let mut settings = settings(EnvironmentProvider::Docker);
        settings.image.dockerfile = Some(DockerfileSource::Path {
            path: "Dockerfile".to_string(),
        });
        let draft = EnvironmentDraft {
            id: EnvironmentId::new("with-dockerfile").unwrap(),
            settings,
        };

        let created = store.create(draft).await.unwrap();
        let persisted =
            fs::read_to_string(dir.path().join("environments").join("with-dockerfile.toml"))
                .await
                .unwrap();

        assert_eq!(
            created.settings.image.dockerfile,
            Some(DockerfileSource::Inline("FROM alpine\n".to_string()))
        );
        assert!(persisted.contains("FROM alpine"));
        assert!(!persisted.contains("path ="));
    }
}
