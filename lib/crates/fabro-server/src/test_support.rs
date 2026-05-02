use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::Request;
#[cfg(test)]
use axum::extract::State as AxumState;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, middleware};
use chrono::Duration as ChronoDuration;
use fabro_config::{RunLayer, RunSettingsBuilder, ServerSettingsBuilder, envfile};
use fabro_interview::Interviewer;
use fabro_static::EnvVars;
use fabro_store::{ArtifactStore, Database};
use fabro_types::settings::ServerAuthMethod;
use fabro_types::{AuthMethod, IdpIdentity, ServerSettings};
use fabro_util::error::SharedError;
use fabro_workflow::handler::HandlerRegistry;
use object_store::memory::InMemory as MemoryObjectStore;
use ulid::Ulid;

use crate::auth;
use crate::ip_allowlist::IpAllowlistConfig;
use crate::jwt_auth::{AuthMode, ConfiguredAuth};
#[cfg(test)]
use crate::principal_middleware::{AuthContextSlot, RequestAuthContext};
use crate::server::{
    self, AppState, AppStateConfig, EnvLookup, RegistryFactoryOverride, ResolvedAppStateSettings,
    RouterOptions, build_app_state, process_env_var,
};
use crate::server_secrets::ServerSecrets;

pub const TEST_DEV_TOKEN: &str =
    "fabro_dev_abababababababababababababababababababababababababababababababab";
pub const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
    server_settings:           ServerSettings,
    manifest_run_defaults:     RunLayer,
    max_concurrent_runs:       usize,
    registry_factory_override: Option<Box<RegistryFactoryOverride>>,
    store_bundle:              Option<(Arc<Database>, ArtifactStore)>,
    vault_path:                Option<PathBuf>,
    server_env_path:           Option<PathBuf>,
    server_secret_env:         HashMap<String, String>,
    env_lookup:                EnvLookup,
}

impl Default for TestAppStateBuilder {
    fn default() -> Self {
        Self {
            server_settings:           default_test_server_settings(),
            manifest_run_defaults:     RunLayer::default(),
            max_concurrent_runs:       5,
            registry_factory_override: None,
            store_bundle:              None,
            vault_path:                None,
            server_env_path:           None,
            server_secret_env:         HashMap::new(),
            env_lookup:                default_env_lookup(),
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

    pub fn registry_factory(
        mut self,
        registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.registry_factory_override = Some(Box::new(registry_factory_override));
        self
    }

    pub fn env_lookup(
        mut self,
        env_lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.env_lookup = Arc::new(env_lookup);
        self
    }

    pub fn server_secret_env(mut self, server_secret_env: HashMap<String, String>) -> Self {
        self.server_secret_env = server_secret_env;
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

    fn vault_path(mut self, vault_path: PathBuf) -> Self {
        self.vault_path = Some(vault_path);
        self
    }

    pub fn build(self) -> Arc<AppState> {
        let (store, artifact_store) = self.store_bundle.unwrap_or_else(test_store_bundle);
        let vault_path = self.vault_path.unwrap_or_else(test_secret_store_path);
        let server_env_path = self
            .server_env_path
            .unwrap_or_else(|| vault_path.with_file_name("server.env"));
        build_app_state(AppStateConfig {
            resolved_settings: resolved_runtime_settings_for_tests(
                self.server_settings,
                self.manifest_run_defaults,
            ),
            registry_factory_override: self.registry_factory_override,
            max_concurrent_runs: self.max_concurrent_runs,
            store,
            artifact_store,
            vault_path,
            server_secrets: load_test_server_secrets(server_env_path, self.server_secret_env),
            env_lookup: self.env_lookup,
            github_api_base_url: None,
            http_client: Some(
                fabro_http::test_http_client().expect("test HTTP client should build"),
            ),
        })
        .expect("test app state should build")
    }
}

pub fn test_app_state() -> Arc<AppState> {
    TestAppStateBuilder::new().build()
}

pub fn test_app_state_with_registry_factory(
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .registry_factory(registry_factory_override)
        .build()
}

pub fn test_app_state_with_settings_and_registry_factory(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .registry_factory(registry_factory_override)
        .build()
}

pub fn test_app_state_with_options_and_registry_factory(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .registry_factory(registry_factory_override)
        .build()
}

pub fn test_app_state_with_options(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .build()
}

pub(crate) fn resolved_runtime_settings_for_tests(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
) -> ResolvedAppStateSettings {
    ResolvedAppStateSettings {
        manifest_run_settings: RunSettingsBuilder::from_run_layer(&manifest_run_defaults)
            .map_err(|err| SharedError::new(anyhow::Error::new(err))),
        manifest_run_defaults,
        server_settings,
    }
}

pub fn test_app_state_with_runtime_settings_and_registry_factory(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .registry_factory(registry_factory_override)
        .build()
}

pub fn test_app_state_with_runtime_settings_and_options_and_registry_factory(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    registry_factory_override: impl Fn(Arc<dyn Interviewer>) -> HandlerRegistry + Send + Sync + 'static,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .registry_factory(registry_factory_override)
        .build()
}

pub fn test_app_state_with_runtime_settings_and_options(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
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

pub fn test_app_state_with_env_lookup_and_server_secret_env(
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
    TestAppStateBuilder::new()
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
    TestAppStateBuilder::new()
        .runtime_settings(server_settings, manifest_run_defaults)
        .max_concurrent_runs(max_concurrent_runs)
        .store_bundle(store, artifact_store)
        .build()
}

pub fn test_store_bundle() -> (Arc<Database>, ArtifactStore) {
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(MemoryObjectStore::new());
    let store = Arc::new(fabro_store::Database::new(
        Arc::clone(&object_store),
        "",
        Duration::from_millis(1),
        None,
    ));
    let artifact_store = ArtifactStore::new(object_store, "artifacts");
    (store, artifact_store)
}

pub fn test_app_state_with_store_and_runtime_settings(
    server_settings: ServerSettings,
    manifest_run_defaults: RunLayer,
    max_concurrent_runs: usize,
    store: Arc<Database>,
    artifact_store: ArtifactStore,
) -> Arc<AppState> {
    TestAppStateBuilder::new()
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

pub fn test_secret_store_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fabro-test-{}", Ulid::new()));
    std::fs::create_dir_all(&dir).expect("test temp dir should be creatable");
    dir.join("secrets.json")
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

pub fn build_test_router_with_options(
    state: Arc<AppState>,
    ip_allowlist_config: Arc<IpAllowlistConfig>,
    options: RouterOptions,
) -> Router {
    with_test_user(server::build_router_with_options(
        state,
        &test_auth_mode(),
        ip_allowlist_config,
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
