use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_config::{
    EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer, EnvironmentLifecycleLayer,
    EnvironmentNetworkLayer, EnvironmentResourcesLayer, MergeMap, StickyMap,
};
use fabro_db::DbPool;
use fabro_types::settings::run::{DockerfileSource, EnvironmentProvider, EnvironmentSettings};
use fabro_types::settings::{Duration, InterpString, Size};
use serde::de::DeserializeOwned;
use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::info;

use crate::{
    Environment, EnvironmentDraft, EnvironmentId, EnvironmentRevision, EnvironmentStoreError,
    EnvironmentValidationError,
};

/// Built-in default environment seeded by install/test setup. The server itself
/// never seeds during normal startup: an uninstalled instance has no persisted
/// managed environments, and a run that selects an absent environment fails
/// explicitly. `local` is intentionally absent from SQLite because it is a
/// reserved, in-memory environment.
const DEFAULT_ENVIRONMENT_ID: &str = "default";

/// `local` is a reserved environment: it is synthesized in memory only when the
/// local sandbox provider is enabled, is never persisted, and cannot be
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
    pool:      DbPool,
    mutations: Mutex<()>,
    state:     std::sync::RwLock<CatalogState>,
}

#[derive(Debug, Clone)]
pub struct ImportReport {
    pub source_path:     PathBuf,
    pub backup_path:     PathBuf,
    pub imported_rows:   i64,
    pub skipped_rows:    i64,
    pub environment_ids: Vec<String>,
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

    fn insert(&mut self, environment: Environment) {
        self.environments
            .insert(environment.id.clone(), environment);
        self.rebuild_catalog();
    }

    fn remove(&mut self, id: &EnvironmentId) {
        self.environments.remove(id);
        self.rebuild_catalog();
    }

    fn rebuild_catalog(&mut self) {
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
    /// Load all persisted environments from SQLite and build the synchronous
    /// in-memory catalog cache used by request paths.
    pub async fn load(pool: DbPool, local_enabled: bool) -> Result<Self, EnvironmentStoreError> {
        let environments = load_environments(&pool, local_enabled).await?;
        Ok(Self {
            pool,
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
        let environment = Environment::from_settings(id.clone(), &settings)?;

        let _mutation = self.mutations.lock().await;
        let mut transaction = self.pool.begin().await?;
        if !insert_environment_ignoring_conflict(&mut transaction, &environment).await? {
            return Err(EnvironmentStoreError::AlreadyExists { id });
        }
        transaction.commit().await?;
        self.write_state().insert(environment.clone());
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
        let environment = Environment::from_settings(id.clone(), &settings)?;

        let _mutation = self.mutations.lock().await;
        let mut transaction = self.pool.begin().await?;
        update_environment(&mut transaction, &environment, expected).await?;
        transaction.commit().await?;
        self.write_state().insert(environment.clone());
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
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query("DELETE FROM environments WHERE id = ? AND revision = ?")
            .bind(id.as_str())
            .bind(expected.as_str())
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() == 0 {
            return Err(revision_mismatch_error(&mut transaction, id, expected).await?);
        }
        transaction.commit().await?;
        self.write_state().remove(id);
        Ok(())
    }

    pub fn catalog_layer(&self) -> Arc<MergeMap<EnvironmentLayer>> {
        Arc::clone(&self.read_state().catalog)
    }
}

async fn load_environments(
    pool: &DbPool,
    local_enabled: bool,
) -> Result<HashMap<EnvironmentId, Environment>, EnvironmentStoreError> {
    let rows = sqlx::query(
        r"
        SELECT
            id,
            revision,
            provider,
            cwd,
            image_docker,
            image_dockerfile_inline,
            resources_cpu,
            resources_memory,
            resources_disk,
            network_mode,
            network_allow_json,
            lifecycle_preserve,
            lifecycle_stop_on_terminal,
            lifecycle_auto_stop,
            labels_json,
            env_json
        FROM environments
        ORDER BY id
        ",
    )
    .fetch_all(pool)
    .await?;

    let mut environments = HashMap::new();
    for row in rows {
        let environment = environment_from_row(&row)?;
        environments.insert(environment.id.clone(), environment);
    }
    if local_enabled {
        let local = synthetic_local_environment()?;
        environments.insert(local.id.clone(), local);
    }
    Ok(environments)
}

fn environment_from_row(row: &SqliteRow) -> Result<Environment, EnvironmentStoreError> {
    let id_text = row.get::<String, _>("id");
    let id = EnvironmentId::new(id_text)?;
    let revision_text = row.get::<String, _>("revision");
    let revision = EnvironmentRevision::from_str(&revision_text).map_err(|source| {
        EnvironmentStoreError::InvalidRevision {
            id: id.clone(),
            source,
        }
    })?;
    let network_allow_json = row.get::<String, _>("network_allow_json");
    let labels_json = row.get::<String, _>("labels_json");
    let env_json = row.get::<String, _>("env_json");
    let layer = EnvironmentLayer {
        provider:  Some(row.get("provider")),
        cwd:       row.get("cwd"),
        image:     image_layer_from_row(row),
        resources: resources_layer_from_row(row)?,
        network:   Some(EnvironmentNetworkLayer {
            mode:  Some(row.get("network_mode")),
            allow: decode_json("network_allow_json", &network_allow_json)?,
        }),
        lifecycle: Some(EnvironmentLifecycleLayer {
            preserve:         Some(row.get("lifecycle_preserve")),
            stop_on_terminal: Some(row.get("lifecycle_stop_on_terminal")),
            auto_stop:        parse_duration(
                "lifecycle_auto_stop",
                row.get("lifecycle_auto_stop"),
            )?,
        }),
        labels:    StickyMap::from(decode_json::<HashMap<String, String>>(
            "labels_json",
            &labels_json,
        )?),
        env:       StickyMap::from(decode_env_json(&env_json)?),
    };

    Environment::from_row(id, revision, &layer)
}

fn image_layer_from_row(row: &SqliteRow) -> Option<EnvironmentImageLayer> {
    let docker: Option<String> = row.get("image_docker");
    let dockerfile_inline: Option<String> = row.get("image_dockerfile_inline");
    if docker.is_none() && dockerfile_inline.is_none() {
        return None;
    }
    Some(EnvironmentImageLayer {
        docker,
        dockerfile: dockerfile_inline.map(EnvironmentDockerfileLayer::Inline),
    })
}

fn resources_layer_from_row(
    row: &SqliteRow,
) -> Result<Option<EnvironmentResourcesLayer>, EnvironmentStoreError> {
    let cpu: Option<i32> = row.get("resources_cpu");
    let memory = parse_size("resources_memory", row.get("resources_memory"))?;
    let disk = parse_size("resources_disk", row.get("resources_disk"))?;
    if cpu.is_none() && memory.is_none() && disk.is_none() {
        return Ok(None);
    }
    Ok(Some(EnvironmentResourcesLayer { cpu, memory, disk }))
}

fn parse_optional_field<T>(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<T>, EnvironmentValidationError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .map(|value| {
            value
                .parse()
                .map_err(|err| EnvironmentValidationError::InvalidSettings {
                    errors: vec![format!("environment.{field}: {err}")],
                })
        })
        .transpose()
}

fn parse_size(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<Size>, EnvironmentValidationError> {
    parse_optional_field(field, value)
}

fn parse_duration(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<Duration>, EnvironmentValidationError> {
    parse_optional_field(field, value)
}

async fn current_revision(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &EnvironmentId,
) -> Result<Option<EnvironmentRevision>, EnvironmentStoreError> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT revision FROM environments WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&mut **transaction)
            .await?;
    current
        .map(|revision| {
            EnvironmentRevision::from_str(&revision).map_err(|source| {
                EnvironmentStoreError::InvalidRevision {
                    id: id.clone(),
                    source,
                }
            })
        })
        .transpose()
}

async fn revision_mismatch_error(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &EnvironmentId,
    expected: &EnvironmentRevision,
) -> Result<EnvironmentStoreError, EnvironmentStoreError> {
    let Some(actual) = current_revision(transaction, id).await? else {
        return Err(EnvironmentStoreError::NotFound { id: id.clone() });
    };
    Ok(EnvironmentStoreError::StaleRevision {
        id: id.clone(),
        expected: expected.clone(),
        actual,
    })
}

async fn insert_environment_ignoring_conflict(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    environment: &Environment,
) -> Result<bool, EnvironmentStoreError> {
    let result = execute_environment_insert_sql(
        transaction,
        environment,
        INSERT_ENVIRONMENT_IGNORE_CONFLICT_SQL,
    )
    .await?;
    Ok(result > 0)
}

async fn execute_environment_insert_sql(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    environment: &Environment,
    sql: &'static str,
) -> Result<u64, EnvironmentStoreError> {
    let row = EnvironmentSqlRow::from_environment(environment)?;
    let result = sqlx::query(sql)
        .bind(row.id)
        .bind(row.revision)
        .bind(row.provider)
        .bind(row.cwd)
        .bind(row.image_docker)
        .bind(row.image_dockerfile_inline)
        .bind(row.resources_cpu)
        .bind(row.resources_memory)
        .bind(row.resources_disk)
        .bind(row.network_mode)
        .bind(row.network_allow_json)
        .bind(row.lifecycle_preserve)
        .bind(row.lifecycle_stop_on_terminal)
        .bind(row.lifecycle_auto_stop)
        .bind(row.labels_json)
        .bind(row.env_json)
        .execute(&mut **transaction)
        .await?;
    Ok(result.rows_affected())
}

async fn update_environment(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    environment: &Environment,
    expected: &EnvironmentRevision,
) -> Result<(), EnvironmentStoreError> {
    let row = EnvironmentSqlRow::from_environment(environment)?;
    let result = sqlx::query(UPDATE_ENVIRONMENT_SQL)
        .bind(row.revision)
        .bind(row.provider)
        .bind(row.cwd)
        .bind(row.image_docker)
        .bind(row.image_dockerfile_inline)
        .bind(row.resources_cpu)
        .bind(row.resources_memory)
        .bind(row.resources_disk)
        .bind(row.network_mode)
        .bind(row.network_allow_json)
        .bind(row.lifecycle_preserve)
        .bind(row.lifecycle_stop_on_terminal)
        .bind(row.lifecycle_auto_stop)
        .bind(row.labels_json)
        .bind(row.env_json)
        .bind(row.id)
        .bind(expected.as_str())
        .execute(&mut **transaction)
        .await?;
    if result.rows_affected() == 0 {
        return Err(revision_mismatch_error(transaction, &environment.id, expected).await?);
    }
    Ok(())
}

const INSERT_ENVIRONMENT_IGNORE_CONFLICT_SQL: &str = r"
INSERT INTO environments (
    id,
    revision,
    provider,
    cwd,
    image_docker,
    image_dockerfile_inline,
    resources_cpu,
    resources_memory,
    resources_disk,
    network_mode,
    network_allow_json,
    lifecycle_preserve,
    lifecycle_stop_on_terminal,
    lifecycle_auto_stop,
    labels_json,
    env_json
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO NOTHING
";

const UPDATE_ENVIRONMENT_SQL: &str = r"
UPDATE environments SET
    revision = ?,
    provider = ?,
    cwd = ?,
    image_docker = ?,
    image_dockerfile_inline = ?,
    resources_cpu = ?,
    resources_memory = ?,
    resources_disk = ?,
    network_mode = ?,
    network_allow_json = ?,
    lifecycle_preserve = ?,
    lifecycle_stop_on_terminal = ?,
    lifecycle_auto_stop = ?,
    labels_json = ?,
    env_json = ?
WHERE id = ? AND revision = ?
";

struct EnvironmentSqlRow {
    id: String,
    revision: String,
    provider: String,
    cwd: Option<String>,
    image_docker: Option<String>,
    image_dockerfile_inline: Option<String>,
    resources_cpu: Option<i32>,
    resources_memory: Option<String>,
    resources_disk: Option<String>,
    network_mode: String,
    network_allow_json: String,
    lifecycle_preserve: bool,
    lifecycle_stop_on_terminal: bool,
    lifecycle_auto_stop: Option<String>,
    labels_json: String,
    env_json: String,
}

impl EnvironmentSqlRow {
    fn from_environment(environment: &Environment) -> Result<Self, EnvironmentStoreError> {
        let settings = &environment.settings;
        let image_dockerfile_inline = match &settings.image.dockerfile {
            Some(DockerfileSource::Inline(value)) => Some(value.clone()),
            Some(DockerfileSource::Path { .. }) => {
                return Err(EnvironmentValidationError::DockerfilePathUnsupported.into());
            }
            None => None,
        };
        Ok(Self {
            id: environment.id.to_string(),
            revision: environment.revision.to_string(),
            provider: settings.provider.to_string(),
            cwd: settings.cwd.clone(),
            image_docker: settings.image.docker.clone(),
            image_dockerfile_inline,
            resources_cpu: settings.resources.cpu,
            resources_memory: settings.resources.memory.map(|size| size.to_string()),
            resources_disk: settings.resources.disk.map(|size| size.to_string()),
            network_mode: settings.network.mode.to_string(),
            network_allow_json: encode_json("network_allow_json", &settings.network.allow)?,
            lifecycle_preserve: settings.lifecycle.preserve,
            lifecycle_stop_on_terminal: settings.lifecycle.stop_on_terminal,
            lifecycle_auto_stop: settings
                .lifecycle
                .auto_stop
                .map(|duration| duration.to_string()),
            labels_json: encode_string_map_json("labels_json", &settings.labels)?,
            env_json: encode_env_json(&settings.env)?,
        })
    }
}

pub async fn seed_environments(pool: &DbPool) -> Result<(), EnvironmentStoreError> {
    seed_default_environment(pool, EnvironmentProvider::Docker).await
}

pub async fn seed_default_environment(
    pool: &DbPool,
    provider: EnvironmentProvider,
) -> Result<(), EnvironmentStoreError> {
    let content = match provider {
        EnvironmentProvider::Docker => DEFAULT_ENVIRONMENT_TOML,
        EnvironmentProvider::Daytona => DAYTONA_DEFAULT_ENVIRONMENT_TOML,
        EnvironmentProvider::Local => LOCAL_ENVIRONMENT_TOML,
    };
    let layer: EnvironmentLayer = toml::from_str(content).map_err(|source| {
        EnvironmentStoreError::parse(PathBuf::from("built-in-default-environment.toml"), source)
    })?;
    let settings =
        fabro_config::resolve_environment_layer(&layer, "environment").map_err(|errors| {
            EnvironmentValidationError::InvalidSettings {
                errors: errors.into_iter().map(|err| err.to_string()).collect(),
            }
        })?;
    let environment = Environment::from_settings(
        EnvironmentId::new(DEFAULT_ENVIRONMENT_ID).expect("default environment id is valid"),
        &settings,
    )?;
    let mut transaction = pool.begin().await?;
    insert_environment_ignoring_conflict(&mut transaction, &environment).await?;
    transaction.commit().await?;

    Ok(())
}

pub async fn import_legacy_directory_once(
    pool: &DbPool,
    source_dir: impl AsRef<Path>,
) -> Result<Option<ImportReport>, EnvironmentStoreError> {
    let source_dir = source_dir.as_ref();
    let paths = legacy_environment_paths(source_dir).await?;
    let Some(paths) = paths else {
        return Ok(None);
    };
    let existing_ids = existing_environment_ids(pool).await?;
    let candidates = read_legacy_environment_directory(paths, &existing_ids).await?;

    let mut transaction = pool.begin().await?;
    let mut imported_ids = Vec::new();
    let mut skipped_rows = candidates.skipped_rows;
    for environment in &candidates.environments {
        if !insert_environment_ignoring_conflict(&mut transaction, environment).await? {
            skipped_rows += 1;
            continue;
        }
        imported_ids.push(environment.id.to_string());
    }
    transaction.commit().await?;

    let backup_path = rename_imported_legacy_directory(source_dir).await?;
    let report = ImportReport {
        source_path: source_dir.to_path_buf(),
        backup_path,
        imported_rows: row_count(imported_ids.len())?,
        skipped_rows: row_count(skipped_rows)?,
        environment_ids: imported_ids,
    };

    info!(
        source_path = %source_dir.display(),
        backup_path = %report.backup_path.display(),
        imported_rows = report.imported_rows,
        skipped_rows = report.skipped_rows,
        environment_ids = ?report.environment_ids,
        "imported legacy environments directory into sqlite"
    );

    Ok(Some(report))
}

struct LegacyCandidates {
    environments: Vec<Environment>,
    skipped_rows: usize,
}

struct LegacyEnvironmentPath {
    id:   EnvironmentId,
    path: PathBuf,
}

async fn legacy_environment_paths(
    source_dir: &Path,
) -> Result<Option<Vec<LegacyEnvironmentPath>>, EnvironmentStoreError> {
    let mut entries = match fs::read_dir(source_dir).await {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(EnvironmentStoreError::io(source_dir, source)),
    };

    let mut paths = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| EnvironmentStoreError::io(source_dir, source))?
    {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .await
            .map_err(|source| EnvironmentStoreError::io(&path, source))?;
        if file_type.is_file() && is_toml_file(&path) {
            paths.push(LegacyEnvironmentPath {
                id: id_from_path(&path)?,
                path,
            });
        }
    }
    paths.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(paths))
}

async fn read_legacy_environment_directory(
    paths: Vec<LegacyEnvironmentPath>,
    existing_ids: &HashSet<EnvironmentId>,
) -> Result<LegacyCandidates, EnvironmentStoreError> {
    let mut environments = Vec::new();
    let mut skipped_rows = 0usize;
    for LegacyEnvironmentPath { id, path } in paths {
        if id.as_str() == RESERVED_LOCAL_ID || existing_ids.contains(&id) {
            skipped_rows += 1;
            continue;
        }
        let bytes = fs::read(&path)
            .await
            .map_err(|source| EnvironmentStoreError::io(&path, source))?;
        let environment = Environment::from_legacy_path(id, &bytes, &path).await?;
        environments.push(environment);
    }
    environments.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(LegacyCandidates {
        environments,
        skipped_rows,
    })
}

async fn existing_environment_ids(
    pool: &DbPool,
) -> Result<HashSet<EnvironmentId>, EnvironmentStoreError> {
    let rows = sqlx::query_scalar::<_, String>("SELECT id FROM environments")
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|id| EnvironmentId::new(id).map_err(EnvironmentStoreError::from))
        .collect()
}

async fn rename_imported_legacy_directory(
    source_dir: &Path,
) -> Result<PathBuf, EnvironmentStoreError> {
    let backup_path = legacy_backup_path(source_dir, Utc::now());
    fs::rename(source_dir, &backup_path)
        .await
        .map_err(|source| EnvironmentStoreError::io(&backup_path, source))?;
    Ok(backup_path)
}

fn legacy_backup_path(source_dir: &Path, imported_at: DateTime<Utc>) -> PathBuf {
    let timestamp = imported_at.format("%Y%m%dT%H%M%S%fZ");
    let mut file_name = source_dir
        .file_name()
        .map_or_else(|| OsString::from("environments"), OsString::from);
    file_name.push(format!(".imported-{timestamp}.bak"));
    source_dir.with_file_name(file_name)
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

fn encode_json<T: serde::Serialize>(
    field: &'static str,
    value: &T,
) -> Result<String, EnvironmentStoreError> {
    serde_json::to_string(value)
        .map_err(|source| EnvironmentStoreError::JsonEncode { field, source })
}

fn decode_json<T: DeserializeOwned>(
    field: &'static str,
    value: &str,
) -> Result<T, EnvironmentStoreError> {
    serde_json::from_str(value)
        .map_err(|source| EnvironmentStoreError::JsonDecode { field, source })
}

fn encode_string_map_json(
    field: &'static str,
    map: &HashMap<String, String>,
) -> Result<String, EnvironmentStoreError> {
    let ordered = map
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<BTreeMap<_, _>>();
    encode_json(field, &ordered)
}

#[expect(
    clippy::disallowed_methods,
    reason = "persisting InterpString source text; resolution happens at consumption time"
)]
fn encode_env_json(map: &HashMap<String, InterpString>) -> Result<String, EnvironmentStoreError> {
    let ordered = map
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_source()))
        .collect::<BTreeMap<_, _>>();
    encode_json("env_json", &ordered)
}

fn decode_env_json(value: &str) -> Result<HashMap<String, InterpString>, EnvironmentStoreError> {
    let decoded = decode_json::<BTreeMap<String, String>>("env_json", value)?;
    Ok(decoded
        .into_iter()
        .map(|(key, value)| (key, InterpString::parse(&value)))
        .collect())
}

fn row_count(count: usize) -> Result<i64, EnvironmentStoreError> {
    i64::try_from(count).map_err(|_| EnvironmentStoreError::RowCountOverflow { count })
}
