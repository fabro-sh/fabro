use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fabro_config::{EnvironmentLayer, MergeMap};
use fabro_types::settings::run::{EnvironmentProvider, EnvironmentSettings};
use tokio::fs;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

use crate::{
    Environment, EnvironmentDraft, EnvironmentId, EnvironmentRevision, EnvironmentStoreError,
};

/// Built-in default environment written to disk by the installer (see
/// [`seed_default_environment`]). The server itself never seeds: a Fabro
/// instance that has not been installed has no managed environments, and a run
/// that selects an absent environment fails explicitly. `local` is
/// intentionally absent: it is a reserved, in-memory environment (see
/// [`RESERVED_LOCAL_ID`]).
const DEFAULT_ENVIRONMENT_ID: &str = "default";

/// `local` is a reserved environment: it is synthesized in memory only when the
/// local sandbox provider is enabled, is never persisted to disk, and cannot be
/// created, replaced, or deleted through the store.
const RESERVED_LOCAL_ID: &str = "local";

/// Returns the built-in seeded environment catalog as a `MergeMap` of
/// `EnvironmentLayer`s. Useful for client-side manifest validation where no
/// live `EnvironmentStore` is available. Includes the reserved `local` entry so
/// manifests selecting `id = "local"` validate; server-side provider-enablement
/// policy decides whether such a run may actually execute.
pub fn seeded_catalog_layer() -> MergeMap<EnvironmentLayer> {
    let mut catalog: HashMap<String, EnvironmentLayer> = HashMap::new();
    let default: EnvironmentLayer =
        toml::from_str(DEFAULT_ENVIRONMENT_TOML).expect("built-in environment seed should parse");
    catalog.insert(DEFAULT_ENVIRONMENT_ID.to_string(), default);
    let local: EnvironmentLayer = toml::from_str(LOCAL_ENVIRONMENT_TOML)
        .expect("built-in local environment seed should parse");
    catalog.insert(RESERVED_LOCAL_ID.to_string(), local);
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

const DAYTONA_DEFAULT_ENVIRONMENT_TOML: &str = r#"provider = "daytona"

[image]
dockerfile = "FROM buildpack-deps:noble\n"

[resources]
cpu = 2
memory = "4GB"

[lifecycle]
preserve = false
stop_on_terminal = true
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

/// Builds the reserved, in-memory `local` environment. It carries only
/// `provider = "local"`; image/resources/network/etc. are irrelevant to the
/// local sandbox and stay at their defaults.
fn synthetic_local_environment() -> Result<Environment, EnvironmentStoreError> {
    let id = EnvironmentId::new(RESERVED_LOCAL_ID).expect("reserved local id is valid");
    let settings = EnvironmentSettings {
        provider: EnvironmentProvider::Local,
        ..EnvironmentSettings::default()
    };
    Environment::synthetic(id, &settings)
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
    /// Synchronously load all persisted environments. The synchronous file
    /// access runs during server startup before request handling begins.
    ///
    /// The server never seeds the default environment; seeding is an
    /// install-time action (see [`seed_default_environment`]). An uninstalled
    /// instance therefore
    /// has no managed environments on disk, and the reserved `local`
    /// environment is the only entry present (when the local provider is
    /// enabled).
    pub fn load(
        dir: impl Into<PathBuf>,
        local_enabled: bool,
    ) -> Result<Self, EnvironmentStoreError> {
        let dir = dir.into();
        let mut environments = load_environments(&dir)?;
        if local_enabled {
            let local = synthetic_local_environment()?;
            environments.insert(local.id.clone(), local);
        }
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
        if id.as_str() == RESERVED_LOCAL_ID {
            return Err(EnvironmentStoreError::Reserved { id });
        }
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
        if id.as_str() == RESERVED_LOCAL_ID {
            return Err(EnvironmentStoreError::Reserved { id: id.clone() });
        }
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
        // `default` is an ordinary deletable environment. Deleting it removes the
        // run fallback, which is intentional: a run that selects `default` after
        // it is gone fails explicitly rather than silently using a built-in.
        if id.as_str() == RESERVED_LOCAL_ID {
            return Err(EnvironmentStoreError::Reserved { id: id.clone() });
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

/// Writes the built-in Docker default environment into `dir`, creating the
/// directory if needed. Existing files are left untouched, so this is
/// idempotent and never clobbers operator edits. Called by legacy installer
/// paths; the running server does not seed.
pub fn seed_environments(dir: &Path) -> Result<(), EnvironmentStoreError> {
    seed_default_environment(dir, EnvironmentProvider::Docker)
}

/// Writes the selected built-in `default` environment into `dir`, creating the
/// directory if needed. Existing files are left untouched, so this is
/// idempotent and never clobbers operator edits. Called by the installer; the
/// running server does not seed.
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "Install-time environment seeding runs synchronously from the installer before the server starts."
)]
pub fn seed_default_environment(
    dir: &Path,
    provider: EnvironmentProvider,
) -> Result<(), EnvironmentStoreError> {
    std::fs::create_dir_all(dir).map_err(|err| EnvironmentStoreError::io(dir, err))?;
    let content = match provider {
        EnvironmentProvider::Docker => DEFAULT_ENVIRONMENT_TOML,
        EnvironmentProvider::Daytona => DAYTONA_DEFAULT_ENVIRONMENT_TOML,
        EnvironmentProvider::Local => LOCAL_ENVIRONMENT_TOML,
    };
    let path = dir.join(format!("{DEFAULT_ENVIRONMENT_ID}.toml"));
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
        // `local` is reserved and synthesized in memory; never load a stale
        // `local.toml` left behind by an earlier build that seeded it.
        if id_from_path(&path).is_ok_and(|id| id.as_str() == RESERVED_LOCAL_ID) {
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
            cwd: None,
            image: EnvironmentImageSettings::default(),
            resources: EnvironmentResourcesSettings::default(),
            network: EnvironmentNetworkSettings::default(),
            lifecycle: EnvironmentLifecycleSettings::default(),
            labels: HashMap::new(),
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
        for id in ["default", "local"] {
            assert!(inner.contains_key(id), "missing {id}");
        }
        assert!(!inner.contains_key("docker"));
        assert!(!inner.contains_key("daytona"));
    }

    #[tokio::test]
    async fn load_does_not_seed_built_ins() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");

        // The server loads without seeding: an uninstalled instance has only the
        // reserved in-memory `local` environment, and nothing is written to disk.
        let store = EnvironmentStore::load(&environment_dir, true).unwrap();
        assert_eq!(
            store
                .list()
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["local"]
        );
        for id in ["default", "docker", "daytona"] {
            assert!(!environment_dir.join(format!("{id}.toml")).exists());
        }
    }

    #[tokio::test]
    async fn seed_environments_writes_default_only_and_load_picks_it_up() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");

        super::seed_environments(&environment_dir).unwrap();
        assert!(environment_dir.join("default.toml").exists());
        assert!(!environment_dir.join("docker.toml").exists());
        assert!(!environment_dir.join("daytona.toml").exists());
        // `local` is reserved and in-memory; it is never written to disk.
        assert!(!environment_dir.join("local.toml").exists());

        let store = EnvironmentStore::load(&environment_dir, true).unwrap();
        assert_eq!(
            store
                .list()
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "local"]
        );
    }

    #[tokio::test]
    async fn seed_environments_is_idempotent_and_preserves_edits() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");

        super::seed_environments(&environment_dir).unwrap();
        // An operator edit to a seeded file must survive a re-seed.
        fs::write(
            environment_dir.join("default.toml"),
            "provider = \"docker\"\n[resources]\ncpu = 7\n",
        )
        .await
        .unwrap();

        super::seed_environments(&environment_dir).unwrap();

        let store = EnvironmentStore::load(&environment_dir, false).unwrap();
        let default = store.get(&EnvironmentId::new("default").unwrap()).unwrap();
        assert_eq!(default.settings.resources.cpu, Some(7));
    }

    #[tokio::test]
    async fn local_present_only_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");

        let enabled = EnvironmentStore::load(&environment_dir, true).unwrap();
        assert!(enabled.get(&EnvironmentId::new("local").unwrap()).is_some());

        let disabled = EnvironmentStore::load(&environment_dir, false).unwrap();
        assert!(
            disabled
                .get(&EnvironmentId::new("local").unwrap())
                .is_none()
        );
    }

    #[tokio::test]
    async fn on_disk_local_is_ignored_in_favor_of_synthetic() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        fs::create_dir_all(&environment_dir).await.unwrap();
        // A stale `local.toml` left by an earlier build that seeded it.
        fs::write(
            environment_dir.join("local.toml"),
            "provider = \"local\"\n[resources]\ncpu = 99\n",
        )
        .await
        .unwrap();

        let store = EnvironmentStore::load(&environment_dir, true).unwrap();
        let local = store.get(&EnvironmentId::new("local").unwrap()).unwrap();

        // The synthetic local carries no resources; the stale file was ignored.
        assert_eq!(local.settings.resources.cpu, None);
    }

    #[tokio::test]
    async fn local_mutations_are_reserved() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
        let local = EnvironmentId::new("local").unwrap();
        let revision = store.get(&local).unwrap().revision;

        let create_err = store
            .create(draft("local", EnvironmentProvider::Local))
            .await
            .unwrap_err();
        assert!(matches!(create_err, EnvironmentStoreError::Reserved { .. }));

        let replace_err = store
            .replace(&local, &revision, settings(EnvironmentProvider::Local))
            .await
            .unwrap_err();
        assert!(matches!(
            replace_err,
            EnvironmentStoreError::Reserved { .. }
        ));

        let delete_err = store.delete(&local, &revision).await.unwrap_err();
        assert!(matches!(delete_err, EnvironmentStoreError::Reserved { .. }));
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

        let store = EnvironmentStore::load(&environment_dir, true).unwrap();

        assert_eq!(
            store
                .list()
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "local", "z"]
        );
    }

    #[test]
    fn invalid_id_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(environment_dir.join("Bad.toml"), r#"provider = "local""#).unwrap();

        let err = EnvironmentStore::load(&environment_dir, true).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::InvalidFilename { .. }));
    }

    #[test]
    fn invalid_provider_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(environment_dir.join("bad.toml"), r#"provider = "bogus""#).unwrap();

        let err = EnvironmentStore::load(&environment_dir, true).unwrap_err();

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

        let err = EnvironmentStore::load(&environment_dir, true).unwrap_err();

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

        let err = EnvironmentStore::load(&environment_dir, true).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        assert!(err.to_string().contains("Dockerfile"));
    }

    #[tokio::test]
    async fn create_conflict_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
        store
            .create(draft("docker", EnvironmentProvider::Docker))
            .await
            .unwrap();

        let err = store
            .create(draft("docker", EnvironmentProvider::Docker))
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::AlreadyExists { .. }));
    }

    #[tokio::test]
    async fn create_invalid_settings_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
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
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
        let current = store
            .create(draft("docker", EnvironmentProvider::Docker))
            .await
            .unwrap();
        let stale = EnvironmentRevision::from_bytes(b"stale");

        let err = store
            .replace(&current.id, &stale, settings(EnvironmentProvider::Docker))
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::StaleRevision { .. }));
    }

    #[tokio::test]
    async fn default_is_deletable() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        super::seed_environments(&environment_dir).unwrap();
        let store = EnvironmentStore::load(&environment_dir, true).unwrap();
        let default = store.get(&EnvironmentId::new("default").unwrap()).unwrap();

        // `default` is an ordinary environment: deleting it succeeds and removes
        // the run fallback rather than being protected.
        store.delete(&default.id, &default.revision).await.unwrap();

        assert!(store.get(&default.id).is_none());
        assert!(!environment_dir.join("default.toml").exists());
    }

    #[tokio::test]
    async fn delete_success_removes_file_and_memory_entry() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        let store = EnvironmentStore::load(&environment_dir, true).unwrap();
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
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
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
    async fn create_persists_cwd_and_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        let store = EnvironmentStore::load(&environment_dir, true).unwrap();
        let mut settings = settings(EnvironmentProvider::Local);
        settings.cwd = Some("/srv/fabro/workspaces/team-a".to_string());

        let created = store
            .create(EnvironmentDraft {
                id: EnvironmentId::new("host").unwrap(),
                settings,
            })
            .await
            .unwrap();

        assert_eq!(
            created.settings.cwd.as_deref(),
            Some("/srv/fabro/workspaces/team-a")
        );
        let persisted = fs::read_to_string(environment_dir.join("host.toml"))
            .await
            .unwrap();
        assert!(persisted.contains("cwd = \"/srv/fabro/workspaces/team-a\""));

        let loaded = EnvironmentStore::load(&environment_dir, true).unwrap();
        let host = loaded.get(&EnvironmentId::new("host").unwrap()).unwrap();
        assert_eq!(
            host.settings.cwd.as_deref(),
            Some("/srv/fabro/workspaces/team-a")
        );
    }

    #[tokio::test]
    async fn create_rejects_relative_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
        let mut settings = settings(EnvironmentProvider::Local);
        settings.cwd = Some("relative/workspace".to_string());

        let err = store
            .create(EnvironmentDraft {
                id: EnvironmentId::new("host").unwrap(),
                settings,
            })
            .await
            .unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        let message = err.to_string();
        assert!(
            message.contains("environment.cwd") && message.contains("absolute path"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn load_rejects_empty_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let environment_dir = dir.path().join("environments");
        std::fs::create_dir_all(&environment_dir).unwrap();
        std::fs::write(
            environment_dir.join("bad.toml"),
            r#"
provider = "local"
cwd = ""
"#,
        )
        .unwrap();

        let err = EnvironmentStore::load(&environment_dir, true).unwrap_err();

        assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
        let message = err.to_string();
        assert!(
            message.contains("environment.cwd") && message.contains("must not be empty"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn api_dockerfile_path_is_resolved_relative_to_settings_dir_and_persisted_inline() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n")
            .await
            .unwrap();
        let store = EnvironmentStore::load(dir.path().join("environments"), true).unwrap();
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
