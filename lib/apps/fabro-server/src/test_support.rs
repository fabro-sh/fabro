use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use axum::body::{self, Body};
use axum::extract::Request;
#[cfg(test)]
use axum::extract::State as AxumState;
use axum::http::{HeaderValue, Request as HttpRequest, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, middleware};
use chrono::Duration as ChronoDuration;
use fabro_config::user::default_storage_dir;
use fabro_config::{LlmLayer, RunLayer, ServerSettingsBuilder, Storage, envfile};
use fabro_db::DbPool;
use fabro_llm::lithos_catalog::Catalog;
use fabro_sandbox::SandboxInventory;
use fabro_static::EnvVars;
use fabro_store::{ArtifactStore, Database, test_support as store_test_support};
use fabro_types::settings::ServerAuthMethod;
use fabro_types::{
    AuthMethod, IdpIdentity, SandboxProviderKind, ServerSettings, WorkflowVersion,
    WorkflowVersionId,
};
use fabro_vault::{SecretType, Vault};
use lithos_llm::catalog::ProviderId;
use object_store::memory::InMemory as MemoryObjectStore;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use crate::auth;
pub use crate::automation_materializer::TestAutomationRunMaterializer;
use crate::interp::process_env_var;
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
#[cfg(test)]
use crate::principal_middleware::{AuthContextSlot, RequestAuthContext};
use crate::server::{
    self, AppState, AppStateConfig, EnvLookup, ResolvedAppStateSettings, RouterOptions,
    build_app_state,
};
use crate::server_secrets::ServerSecrets;
#[cfg(test)]
use crate::worker_runtime::WorkerRuntime;

pub const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
pub const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const TEST_OPENAI_API_KEY: &str = "test-openai-api-key";
const FABRO_TEST_ASSUME_LLM_READY: &str = "FABRO_TEST_ASSUME_LLM_READY";

/// Supply enabled catalog providers to CLI fixture run materialization.
///
/// This is scoped to the `test-support` feature and an explicit child-process
/// flag. The CLI suite shares a server whose credential state can change
/// between tests, so the flag deliberately ignores that mutable state. It does
/// not register adapters or make model execution available.
pub(crate) fn test_run_materialization_provider_ids(
    catalog: &Catalog,
    ready_provider_ids: &[ProviderId],
) -> Vec<ProviderId> {
    let assume_ready = process_env_var(FABRO_TEST_ASSUME_LLM_READY)
        .is_some_and(|value| !matches!(value.as_str(), "" | "0" | "false" | "no"));
    if assume_ready {
        catalog.enabled_provider_ids().into_iter().collect()
    } else {
        ready_provider_ids.to_vec()
    }
}

pub fn default_test_server_settings() -> ServerSettings {
    ServerSettingsBuilder::from_toml(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]
"#,
    )
    .expect("default test server settings should resolve")
}

#[must_use]
pub struct TestAppStateBuilder {
    server_settings:              ServerSettings,
    manifest_run_defaults:        RunLayer,
    max_concurrent_runs:          usize,
    execute_in_process:           bool,
    sandbox_inventory:            Option<SandboxInventory>,
    store_bundle:                 Option<(Arc<Database>, ArtifactStore)>,
    vault_path:                   Option<PathBuf>,
    vault_entries:                Vec<(String, String)>,
    server_env_path:              Option<PathBuf>,
    active_config_path:           Option<PathBuf>,
    server_secret_env:            HashMap<String, String>,
    default_environment_provider: Option<SandboxProviderKind>,
    env_lookup:                   EnvLookup,
    llm_overlay:                  LlmLayer,
    automation_materializer:      Option<TestAutomationRunMaterializer>,
    #[cfg(test)]
    worker_runtime:               Option<Arc<dyn WorkerRuntime>>,
}

impl Default for TestAppStateBuilder {
    fn default() -> Self {
        Self {
            server_settings:              default_test_server_settings(),
            manifest_run_defaults:        RunLayer::default(),
            max_concurrent_runs:          5,
            execute_in_process:           false,
            sandbox_inventory:            None,
            store_bundle:                 None,
            vault_path:                   None,
            vault_entries:                Vec::new(),
            server_env_path:              None,
            active_config_path:           None,
            server_secret_env:            HashMap::new(),
            default_environment_provider: Some(SandboxProviderKind::DOCKER),
            env_lookup:                   default_env_lookup(),
            llm_overlay:                  LlmLayer::default(),
            automation_materializer:      None,
            #[cfg(test)]
            worker_runtime:               None,
        }
    }
}

impl TestAppStateBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn runtime_settings(
        mut self,
        server_settings: ServerSettings,
        manifest_run_defaults: RunLayer,
    ) -> Self {
        self.server_settings = server_settings;
        self.manifest_run_defaults = manifest_run_defaults;
        self
    }

    pub fn max_concurrent_runs(mut self, max_concurrent_runs: usize) -> Self {
        self.max_concurrent_runs = max_concurrent_runs;
        self
    }

    /// Execute runs in the server process instead of a worker, so a
    /// scenario needs no worker binary.
    pub fn in_process_execution(mut self) -> Self {
        self.execute_in_process = true;
        self
    }

    pub fn sandbox_inventory(mut self, sandbox_inventory: SandboxInventory) -> Self {
        self.sandbox_inventory = Some(sandbox_inventory);
        self
    }

    pub fn env_lookup(
        mut self,
        env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.env_lookup = Arc::new(env_lookup);
        self
    }

    /// Replaces the operator `[llm]` overlay applied above the built-in and
    /// policy layers.
    pub fn llm_overlay(mut self, overlay: LlmLayer) -> Self {
        self.llm_overlay = overlay;
        self
    }

    /// Parses `toml` as the operator `[llm]` overlay.
    pub fn llm_overlay_toml(self, toml: &str) -> Self {
        self.llm_overlay(llm_overlay_from_toml(toml))
    }

    pub fn automation_materializer(mut self, materializer: TestAutomationRunMaterializer) -> Self {
        self.automation_materializer = Some(materializer);
        self
    }

    #[cfg(test)]
    pub(crate) fn worker_runtime(mut self, worker_runtime: Arc<dyn WorkerRuntime>) -> Self {
        self.worker_runtime = Some(worker_runtime);
        self
    }

    pub fn provider_base_url(
        mut self,
        provider: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let overlay = llm_overlay_with_provider_base_url(provider, base_url);
        let mut merged = toml::Value::Table(std::mem::take(&mut self.llm_overlay).0);
        merge_toml(&mut merged, toml::Value::Table(overlay.0));
        let toml::Value::Table(table) = merged else {
            unreachable!("merging two tables yields a table");
        };
        self.llm_overlay = LlmLayer(table);
        self
    }

    pub fn server_secret_env(mut self, server_secret_env: HashMap<String, String>) -> Self {
        self.server_secret_env = server_secret_env;
        self
    }

    pub fn default_environment_provider(mut self, provider: Option<SandboxProviderKind>) -> Self {
        self.default_environment_provider = provider;
        self
    }

    pub fn store_bundle(mut self, store: Arc<Database>, artifact_store: ArtifactStore) -> Self {
        self.store_bundle = Some((store, artifact_store));
        self
    }

    fn server_env_path(mut self, server_env_path: PathBuf) -> Self {
        self.server_env_path = Some(server_env_path);
        self
    }

    pub fn vault_path(mut self, vault_path: PathBuf) -> Self {
        self.vault_path = Some(vault_path);
        self
    }

    pub fn active_config_path(mut self, active_config_path: PathBuf) -> Self {
        self.active_config_path = Some(active_config_path);
        self
    }

    /// Pre-populate the vault file with optional integration secrets (token
    /// type) before [`build_app_state`] opens it.
    pub fn vault_entries<I, K, V>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.vault_entries
            .extend(entries.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn build(self) -> Arc<AppState> {
        self.try_build().expect("test app state should build")
    }

    pub fn try_build(mut self) -> anyhow::Result<Arc<AppState>> {
        let (store, artifact_store) = self.store_bundle.unwrap_or_else(test_store_bundle);
        let vault_path = self.vault_path.unwrap_or_else(test_secret_store_path);
        self.server_settings = redirect_default_storage_root(self.server_settings, &vault_path)?;
        if !self.vault_entries.is_empty() {
            let mut vault = Vault::load(vault_path.clone()).expect("test vault should load");
            for (name, value) in &self.vault_entries {
                vault
                    .set(name, value, SecretType::Token, None)
                    .expect("test vault entry should persist");
            }
        }
        let server_env_path = self
            .server_env_path
            .unwrap_or_else(|| vault_path.with_file_name("server.env"));
        let active_config_path = self
            .active_config_path
            .unwrap_or_else(|| vault_path.with_file_name("settings.toml"));
        let db_pool = test_db_pool_for_vault_path_with_default_environment(
            &vault_path,
            self.default_environment_provider,
        )?;
        import_test_legacy_automations(
            db_pool.clone(),
            server::automation_dir_for_active_config(&active_config_path),
        )?;
        let preloaded_vault = test_secret_snapshot(db_pool.clone())?;
        let automation_materializer_override = self.automation_materializer.map(|materializer| {
            materializer.into_materializer(fabro_workflow_version::WorkflowVersionStore::new(
                store.blobs(),
            ))
        });
        build_app_state(AppStateConfig {
            resolved_settings: resolved_runtime_settings_for_tests(
                self.server_settings,
                self.manifest_run_defaults,
                self.llm_overlay,
            ),
            execute_in_process: self.execute_in_process,
            max_concurrent_runs: self.max_concurrent_runs,
            store,
            artifact_store,
            db_pool,
            preloaded_vault,
            server_secrets: load_test_server_secrets(server_env_path, self.server_secret_env),
            env_lookup: self.env_lookup,
            github_api_base_url: None,
            active_config_path,
            http_client: Some(
                fabro_http::test_http_client().expect("test HTTP client should build"),
            ),
            sandbox_inventory: self.sandbox_inventory,
            shutdown: CancellationToken::new(),
            #[cfg(test)]
            worker_control_bus: None,
            #[cfg(test)]
            worker_runtime: self.worker_runtime,
            automation_materializer_override,
        })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "sync test builders may run inside Tokio; a dedicated thread avoids a nested runtime"
)]
pub(crate) fn test_secret_snapshot(pool: DbPool) -> anyhow::Result<Vault> {
    std::thread::spawn(move || {
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()?;
        runtime
            .block_on(fabro_vault::SecretStore::new(pool).snapshot())
            .map(fabro_vault::SecretSnapshot::into_vault)
            .map_err(anyhow::Error::new)
    })
    .join()
    .expect("test secret snapshot thread should not panic")
}

/// Merges `overlay` into `base` the way lithos layers merge: tables merge
/// key by key and every other value replaces.
fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge_toml(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

/// Parses `toml` as an operator `[llm]` overlay.
pub fn llm_overlay_from_toml(toml: &str) -> LlmLayer {
    LlmLayer(toml::from_str(toml).expect("test llm overlay should parse"))
}

/// An overlay that points one provider at `base_url`, the way an operator
/// repoints a provider at a proxy or a test double.
pub fn llm_overlay_with_provider_base_url(
    provider: impl Into<String>,
    base_url: impl Into<String>,
) -> LlmLayer {
    let provider = provider.into();
    llm_overlay_from_toml(&format!(
        "[providers.{}]\nbase_url = {}\n",
        toml::Value::String(provider),
        toml::Value::String(base_url.into())
    ))
}

/// The catalog a test app state builds from `overlay`.
pub fn test_catalog_with_overlay(overlay: &LlmLayer) -> Catalog {
    fabro_llm::build_catalog(overlay, &|_| None).expect("test catalog should build")
}

pub fn test_app_state() -> Arc<AppState> {
    ready_test_app_state_builder().build()
}

pub fn test_app_state_in_process() -> Arc<AppState> {
    ready_test_app_state_builder()
        .in_process_execution()
        .build()
}

pub fn test_app_state_with_settings_in_process(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .in_process_execution()
        .build()
}

pub fn test_app_state_with_options_in_process(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .in_process_execution()
        .build()
}

pub fn test_app_state_with_options(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .build()
}

fn ready_test_app_state_builder() -> TestAppStateBuilder {
    TestAppStateBuilder::new().vault_entries([(EnvVars::OPENAI_API_KEY, TEST_OPENAI_API_KEY)])
}

pub(crate) fn resolved_runtime_settings_for_tests(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    llm_overlay: LlmLayer,
) -> ResolvedAppStateSettings {
    ResolvedAppStateSettings {
        server_settings,
        manifest_run_defaults,
        llm_overlay,
    }
}

pub fn test_app_state_with_runtime_settings_in_process(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .in_process_execution()
        .build()
}

pub fn test_app_state_with_runtime_settings_and_options_in_process(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .in_process_execution()
        .build()
}

pub fn test_app_state_with_runtime_settings_and_options(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .build()
}

pub fn test_app_state_with_runtime_settings_and_env_lookup(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .env_lookup(env_lookup)
        .build()
}

pub fn test_app_state_with_runtime_settings_and_env_lookup_and_server_secret_env(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    server_secret_env: &HashMap<String, String>,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .env_lookup(env_lookup)
        .server_secret_env(server_secret_env.clone())
        .build()
}

pub fn test_app_state_with_env_lookup(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .env_lookup(env_lookup)
        .build()
}

#[expect(
    clippy::disallowed_methods,
    reason = "test helper writes a fixture server.env with sync std::fs::write"
)]
pub fn test_app_state_with_runtime_settings_and_session_key(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    session_secret: Option<&str>,
) -> Arc<AppState> {
    let vault_path = test_secret_store_path();
    let server_env_path = vault_path
        .parent()
        .expect("test secrets path should have parent")
        .join("server.env");
    if let Some(session_secret) = session_secret {
        std::fs::write(
            &server_env_path,
            format!("SESSION_SECRET={session_secret}\n"),
        )
        .expect("test server env should be writable");
    }
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .vault_path(vault_path)
        .server_env_path(server_env_path)
        .build()
}

pub fn test_app_state_with_session_key(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    session_secret: Option<&str>,
) -> Arc<AppState> {
    test_app_state_with_runtime_settings_and_session_key(
        server_settings,
        manifest_run_defaults,
        session_secret,
    )
}

pub fn test_app_state_with_store(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    store: Arc<Database>,
    artifact_store: ArtifactStore,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .store_bundle(store, artifact_store)
        .build()
}

pub fn test_store_bundle() -> (Arc<Database>, ArtifactStore) {
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(MemoryObjectStore::new());
    let store = Arc::new(store_test_support::test_database());
    let artifact_store = ArtifactStore::new(object_store, "artifacts");
    (store, artifact_store)
}

#[cfg(test)]
pub(crate) fn test_db_pool_for_vault_path(vault_path: &Path) -> anyhow::Result<DbPool> {
    test_db_pool_for_vault_path_with_default_environment(
        vault_path,
        Some(SandboxProviderKind::DOCKER),
    )
}

pub(crate) fn test_db_pool_for_vault_path_with_default_environment(
    vault_path: &Path,
    default_environment_provider: Option<SandboxProviderKind>,
) -> anyhow::Result<DbPool> {
    test_db_pool(
        sqlite_path_for_vault_path(vault_path),
        vault_path.to_path_buf(),
        default_environment_provider,
    )
}

fn sqlite_path_for_vault_path(vault_path: &Path) -> PathBuf {
    vault_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("db")
        .join("fabro.sqlite3")
}

pub async fn test_environment_from_storage_dir(
    storage_dir: &Path,
    id: &str,
) -> anyhow::Result<Option<fabro_environment::Environment>> {
    let database = fabro_db::Database::connect(Storage::new(storage_dir).sqlite_path()).await?;
    let store = fabro_environment::EnvironmentStore::load(database.clone_pool(), false).await?;
    let id = fabro_environment::EnvironmentId::new(id)?;
    Ok(store.get(&id))
}

#[expect(
    clippy::disallowed_methods,
    reason = "sync test builders may be called inside async tests; a short-lived OS thread avoids nested Tokio runtimes"
)]
fn test_db_pool(
    path: PathBuf,
    vault_path: PathBuf,
    default_environment_provider: Option<SandboxProviderKind>,
) -> anyhow::Result<DbPool> {
    std::thread::spawn(move || {
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async move {
            let database = fabro_db::Database::connect(&path).await?;
            database.migrate().await?;
            fabro_vault::import_legacy_json_once(database.pool(), vault_path).await?;
            if let Some(provider) = default_environment_provider {
                fabro_environment::seed_default_environment(database.pool(), provider).await?;
            }
            Ok(database.clone_pool())
        })
    })
    .join()
    .expect("test database setup thread should not panic")
}

#[expect(
    clippy::disallowed_methods,
    reason = "sync test builders may run inside async tests; a short-lived OS thread avoids nested Tokio runtimes"
)]
fn import_test_legacy_automations(pool: DbPool, source_dir: PathBuf) -> anyhow::Result<()> {
    std::thread::spawn(move || {
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()?;
        runtime
            .block_on(fabro_automation::import_legacy_directory_once(
                &pool, source_dir,
            ))
            .map(|_| ())
            .map_err(anyhow::Error::new)
    })
    .join()
    .expect("test automation import thread should not panic")
}

pub fn test_app_state_with_store_and_runtime_settings(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    store: Arc<Database>,
    artifact_store: ArtifactStore,
) -> Arc<AppState> {
    ready_test_app_state_builder()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .store_bundle(store, artifact_store)
        .build()
}

pub(crate) fn default_env_lookup() -> EnvLookup {
    Arc::new(process_env_var)
}

pub(crate) fn load_test_server_secrets(
    path: PathBuf,
    env: HashMap<String, String>,
) -> ServerSecrets {
    let mut env = env;
    let file_has_session_secret = envfile::read_env_file(&path)
        .ok()
        .is_some_and(|entries| entries.contains_key(EnvVars::SESSION_SECRET));
    if !env.contains_key(EnvVars::SESSION_SECRET) && !file_has_session_secret {
        env.insert(
            EnvVars::SESSION_SECRET.to_string(),
            "server-test-session-key-0123456789".to_string(),
        );
    }
    ServerSecrets::load(path, env).expect("test server secrets should load")
}

/// The database pool the app state's stores share, for a test that reads
/// what a run wrote through another store over the same database.
#[must_use]
pub fn test_app_db_pool(state: &AppState) -> DbPool {
    state.db_pool.clone()
}

pub fn test_secret_store_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fabro-test-{}", Ulid::new()));
    std::fs::create_dir_all(&dir).expect("test temp dir should be creatable");
    dir.join("secrets.json")
}

/// Keeps tests off the developer's real `~/.fabro/storage`.
///
/// Settings built for tests usually omit `[server.storage] root`, which
/// resolves to the production default. Handlers that walk that tree — `df`,
/// `system/resources`, `prune` — then read whatever runs and scratch
/// directories the machine happens to have, making tests slow and
/// machine-dependent, and letting run-creating tests write there.
///
/// Only settings still carrying the production default are redirected; a test
/// that chose its own root keeps it. The redirect goes through
/// [`ServerSettings::with_storage_override`] so the derived local object-store
/// roots move with it instead of pointing back at the real storage tree.
fn redirect_default_storage_root(
    settings: ServerSettings,
    vault_path: &Path,
) -> anyhow::Result<ServerSettings> {
    if Path::new(&settings.server.storage.root) != default_storage_dir() {
        return Ok(settings);
    }
    let root = vault_path.with_file_name("storage");
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating test storage root at {}", root.display()))?;
    Ok(settings.with_storage_override(&root))
}

#[must_use]
pub fn test_auth_mode() -> AuthMode {
    AuthMode::Enabled(ConfiguredAuth {
        methods:    vec![ServerAuthMethod::DevToken, ServerAuthMethod::Github],
        dev_token:  Some(TEST_DEV_TOKEN.to_string()),
        jwt_key:    Some(
            auth::derive_jwt_key(TEST_SESSION_SECRET.as_bytes())
                .expect("test jwt signing key should derive"),
        ),
        jwt_issuer: Some("https://fabro.test".to_string()),
    })
}

pub fn build_test_router(state: Arc<AppState>) -> Router {
    with_test_user(server::build_router(state, test_auth_mode()))
}

pub fn build_test_router_with_options(state: Arc<AppState>, options: RouterOptions) -> Router {
    with_test_user(server::build_router_with_options(
        state,
        &test_auth_mode(),
        options,
    ))
}

pub fn with_test_user(router: Router) -> Router {
    router.layer(middleware::from_fn(inject_test_user_bearer))
}

async fn inject_test_user_bearer(mut req: Request, next: Next) -> Response {
    if req.uri().path().starts_with("/api/") && !req.headers().contains_key(header::AUTHORIZATION) {
        static BEARER: OnceLock<HeaderValue> = OnceLock::new();
        let bearer = BEARER.get_or_init(|| {
            HeaderValue::from_str(&format!("Bearer {}", issue_test_user_token()))
                .expect("test JWT bearer header is valid")
        });
        req.headers_mut()
            .insert(header::AUTHORIZATION, bearer.clone());
    }
    next.run(req).await
}

fn issue_test_user_token() -> String {
    let key = auth::derive_jwt_key(TEST_SESSION_SECRET.as_bytes())
        .expect("test jwt signing key should derive");
    auth::issue(
        &key,
        "https://fabro.test",
        &auth::JwtSubject {
            identity:    IdpIdentity::new("fabro:dev", "dev")
                .expect("test identity should be valid"),
            login:       "dev".to_string(),
            name:        "Dev Token".to_string(),
            email:       "dev@fabro.local".to_string(),
            avatar_url:  String::new(),
            user_url:    String::new(),
            auth_method: AuthMethod::DevToken,
        },
        ChronoDuration::days(3650),
    )
}

#[cfg(test)]
pub(crate) async fn capture_auth_context(
    AxumState(captured): AxumState<Arc<Mutex<Vec<RequestAuthContext>>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let slot = AuthContextSlot::initial();
    req.extensions_mut().insert(slot.clone());
    let response = next.run(req).await;
    captured
        .lock()
        .expect("captured auth contexts lock poisoned")
        .push(slot.snapshot());
    response
}

/// Register fixture content through the same HTTP boundary as run producers.
pub async fn test_register_workflow_version(
    app: &Router,
    version: &WorkflowVersion,
    bearer: Option<&str>,
) -> WorkflowVersionId {
    use tower::ServiceExt as _;

    let mut request = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/workflow-versions")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(version).expect("fixture version should serialize"),
                ))
                .expect("version registration request should build"),
        )
        .await
        .expect("version registration should route");
    let status = response.status();
    let bytes = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("version registration response body should be readable");
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).expect("version registration response should be JSON");
    serde_json::from_value(body["workflow_version_id"].clone())
        .expect("version registration response should contain a valid ID")
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::ObjectStoreSettings;

    use super::*;

    fn local_store_root(store: &ObjectStoreSettings) -> &Path {
        let ObjectStoreSettings::Local { root } = store else {
            panic!("test server settings should use a local object store");
        };
        Path::new(root)
    }

    #[test]
    fn default_storage_redirect_updates_derived_local_store_roots() {
        let temp_dir = tempfile::tempdir().expect("test temp dir should be created");
        let vault_path = temp_dir.path().join("secrets.json");

        let settings = redirect_default_storage_root(default_test_server_settings(), &vault_path)
            .expect("default test storage root should redirect");
        let storage_root = temp_dir.path().join("storage");

        assert_eq!(Path::new(&settings.server.storage.root), storage_root);
        assert_eq!(
            local_store_root(&settings.server.artifacts.store),
            storage_root.join("objects/artifacts")
        );
    }

    #[test]
    fn default_storage_redirect_preserves_explicit_storage_root() {
        let temp_dir = tempfile::tempdir().expect("test temp dir should be created");
        let vault_path = temp_dir.path().join("secrets.json");
        let expected =
            default_test_server_settings().with_storage_override(&temp_dir.path().join("custom"));

        let settings = redirect_default_storage_root(expected.clone(), &vault_path)
            .expect("explicit test storage root should be preserved");

        assert_eq!(settings, expected);
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous fixture setup creates a blocking file before exercising the helper"
    )]
    fn default_storage_redirect_preserves_directory_creation_error_chain() {
        let temp_dir = tempfile::tempdir().expect("test temp dir should be created");
        let vault_path = temp_dir.path().join("secrets.json");
        std::fs::write(temp_dir.path().join("storage"), "not a directory")
            .expect("blocking storage path should be created");

        let error = redirect_default_storage_root(default_test_server_settings(), &vault_path)
            .expect_err("storage redirect should reject a file at the directory path");

        assert!(error.to_string().contains("creating test storage root at"));
        assert!(
            error.chain().count() >= 2,
            "filesystem error should remain in the source chain"
        );
    }
}
