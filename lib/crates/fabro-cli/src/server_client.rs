use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_client::{
    AuthStore, Credential, CredentialFallback, OAuthSession, ServerTarget, TransportConnector,
    apply_bearer_token_auth,
};
pub(crate) use fabro_client::{Client, RunEventStream};
use fabro_config::Storage;
use fabro_server::bind::Bind;
pub(crate) use fabro_types::RunProjection;
use fabro_types::settings::SettingsLayer;
use fabro_util::dev_token::validate_dev_token_format;
use fabro_util::{Home, dev_token};
use tokio::time::sleep;

use crate::args::ServerTargetArgs;
use crate::commands::server::{record, start};
use crate::user_config::{self, cli_http_client_builder};

#[derive(Debug)]
struct CliDevTokenFallback {
    storage_dir: Option<PathBuf>,
}

impl CredentialFallback for CliDevTokenFallback {
    fn resolve(&self) -> Option<Credential> {
        load_dev_token_if_available(self.storage_dir.as_deref()).map(Credential::DevToken)
    }
}

fn refreshable_oauth(
    target: &ServerTarget,
    credential: Option<&Credential>,
) -> Option<OAuthSession> {
    if matches!(credential, Some(Credential::OAuth(_))) {
        let session = OAuthSession::new(target.clone(), AuthStore::default());
        if local_dev_token_fallback(target) {
            return Some(
                session.with_fallback(Arc::new(CliDevTokenFallback { storage_dir: None })),
            );
        }
        return Some(session);
    }
    None
}

pub(crate) async fn connect_server(storage_dir: &Path) -> Result<Client> {
    connect_local_api_client_bundle(storage_dir, &user_config::active_settings_path(None)).await
}

pub(crate) async fn connect_server_target(target: &ServerTarget) -> Result<Client> {
    connect_target_api_client_bundle(target).await
}

pub(crate) async fn connect_server_target_direct(target: &str) -> Result<Client> {
    let target = target.parse::<ServerTarget>()?;
    connect_server_target(&target).await
}

pub(crate) async fn connect_server_with_settings(
    args: &ServerTargetArgs,
    settings: &SettingsLayer,
    base_config_path: &Path,
) -> Result<Client> {
    if let Some(target) = user_config::resolve_nondefault_server_target(args, settings)? {
        if let Some(path) = target.as_unix_socket_path() {
            return connect_managed_unix_socket_api_client_bundle(
                path,
                &user_config::storage_dir(settings)?,
                base_config_path,
            )
            .await;
        }
        return connect_target_api_client_bundle(&target).await;
    }

    connect_local_api_client_bundle(&user_config::storage_dir(settings)?, base_config_path).await
}

async fn connect_managed_unix_socket_api_client_bundle(
    path: &Path,
    storage_dir: &Path,
    active_config_path: &Path,
) -> Result<Client> {
    let target = ServerTarget::unix_socket_path(path)?;
    let credential = resolve_target_credential(
        &target,
        Some(storage_dir),
        local_dev_token_fallback(&target),
    )?;
    let oauth_session = refreshable_oauth(&target, credential.as_ref());
    let bearer_token = credential.as_ref().map(Credential::bearer_token);

    let http_client = if let Ok(http_client) =
        try_connect_unix_socket_http_client(path, Some(storage_dir), bearer_token).await
    {
        http_client
    } else {
        start::ensure_server_running_on_socket(path, active_config_path, storage_dir)
            .await
            .with_context(|| format!("Failed to start fabro server for {}", path.display()))?;
        connect_unix_socket_http_client(path, Some(storage_dir), bearer_token)
            .await
            .with_context(|| format!("Failed to connect to fabro server at {}", path.display()))?
    };

    build_client(
        target,
        credential,
        oauth_session,
        Some(("http://fabro".to_string(), http_client)),
    )
    .await
}

async fn connect_local_api_client_bundle(
    storage_dir: &Path,
    active_config_path: &Path,
) -> Result<Client> {
    let bind = start::ensure_server_running_for_storage(storage_dir, active_config_path)
        .await
        .with_context(|| format!("Failed to start fabro server for {}", storage_dir.display()))?;
    match bind {
        Bind::Unix(path) => {
            let http_client =
                connect_unix_socket_http_client(&path, Some(storage_dir), None).await?;
            Ok(Client::from_http_client("http://fabro", http_client))
        }
        Bind::Tcp(addr) => {
            let token = wait_for_local_dev_token(storage_dir).await?;
            let builder = cli_http_client_builder().no_proxy();
            let http_client = apply_bearer_token_auth(builder, &token)?.build()?;
            let base_url = format!("http://{addr}");
            Ok(Client::from_http_client(base_url, http_client))
        }
    }
}

#[allow(
    dead_code,
    reason = "Retained for pending storage-backed internal callers and referenced in existing design docs."
)]
pub(crate) async fn connect_api_client(storage_dir: &Path) -> Result<fabro_api::ApiClient> {
    connect_local_api_client_bundle(storage_dir, &user_config::active_settings_path(None))
        .await
        .map(|client| client.api_client())
}

async fn connect_target_api_client_bundle(target: &ServerTarget) -> Result<Client> {
    let credential = resolve_target_credential(target, None, local_dev_token_fallback(target))?;
    let oauth_session = refreshable_oauth(target, credential.as_ref());
    build_client(target.clone(), credential, oauth_session, None).await
}

async fn build_client(
    target: ServerTarget,
    credential: Option<Credential>,
    oauth_session: Option<OAuthSession>,
    transport: Option<(String, fabro_http::HttpClient)>,
) -> Result<Client> {
    let mut builder = Client::builder()
        .target(target.clone())
        .transport_connector(build_cli_transport_connector(target));
    if let Some((base_url, http_client)) = transport {
        builder = builder.transport(base_url, http_client);
    }
    if let Some(credential) = credential {
        builder = builder.credential(credential);
    }
    if let Some(oauth_session) = oauth_session {
        builder = builder.oauth_session(oauth_session);
    }
    builder.connect().await
}

fn build_cli_transport_connector(target: ServerTarget) -> TransportConnector {
    TransportConnector::new(move |bearer_token| {
        let target = target.clone();
        async move { connect_cli_target_transport(&target, bearer_token.as_deref()) }
    })
}

fn connect_cli_target_transport(
    target: &ServerTarget,
    bearer_token: Option<&str>,
) -> Result<(fabro_http::HttpClient, String)> {
    if let Some(api_url) = target.as_http_url() {
        let mut builder = cli_http_client_builder();
        builder = match bearer_token {
            Some(token) => apply_bearer_token_auth(builder, token)?,
            None => builder,
        };
        let http_client = builder.build()?;
        return Ok((http_client, api_url.to_string()));
    }

    let Some(path) = target.as_unix_socket_path() else {
        bail!("server target must be an http(s) URL or absolute Unix socket path");
    };
    let mut builder = cli_http_client_builder().unix_socket(path).no_proxy();
    builder = match bearer_token {
        Some(token) => apply_bearer_token_auth(builder, token)?,
        None => builder,
    };
    let http_client = builder
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")?;
    Ok((http_client, "http://fabro".to_string()))
}

fn local_dev_token_fallback(target: &ServerTarget) -> bool {
    target.is_unix_socket()
}

fn load_dev_token_if_available(storage_dir: Option<&Path>) -> Option<String> {
    let env_token = std::env::var("FABRO_DEV_TOKEN").ok();
    load_dev_token_if_available_from_sources(storage_dir, env_token.as_deref(), &Home::from_env())
}

fn load_dev_token_if_available_from_sources(
    storage_dir: Option<&Path>,
    env_token: Option<&str>,
    home: &Home,
) -> Option<String> {
    if let Some(token) = env_token.filter(|token| validate_dev_token_format(token)) {
        return Some(token.to_owned());
    }

    if let Some(storage_dir) = storage_dir {
        let storage_token_path = Storage::new(storage_dir).server_state().dev_token_path();
        if let Some(token) = dev_token::read_dev_token_file(&storage_token_path) {
            return Some(token);
        }

        let record_path = Storage::new(storage_dir).server_state().record_path();
        if let Some(token) = record::read_server_record(&record_path)
            .and_then(|server| server.dev_token_path)
            .as_deref()
            .and_then(dev_token::read_dev_token_file)
        {
            return Some(token);
        }
    }

    dev_token::read_dev_token_file(&home.dev_token_path())
}

async fn wait_for_local_dev_token(storage_dir: &Path) -> Result<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while std::time::Instant::now() < deadline {
        if let Some(token) = load_dev_token_if_available(Some(storage_dir)) {
            return Ok(token);
        }
        sleep(Duration::from_millis(50)).await;
    }

    bail!(
        "local server dev token did not become available for {}",
        storage_dir.display()
    );
}

async fn build_authed_unix_socket_http_client(
    path: &Path,
    storage_dir: Option<&Path>,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    let builder = cli_http_client_builder().unix_socket(path).no_proxy();
    let builder = if let Some(token) = bearer_token {
        apply_bearer_token_auth(builder, token)?
    } else if let Some(storage_dir) = storage_dir {
        let token = wait_for_local_dev_token(storage_dir).await?;
        apply_bearer_token_auth(builder, &token)?
    } else {
        builder
    };

    builder
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")
}

fn build_unix_socket_probe_client(path: &Path) -> Result<fabro_http::HttpClient> {
    cli_http_client_builder()
        .unix_socket(path)
        .no_proxy()
        .build()
        .context("Failed to build Unix-socket HTTP client for fabro server")
}

async fn try_connect_unix_socket_http_client(
    path: &Path,
    storage_dir: Option<&Path>,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    check_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_http_client(path, storage_dir, bearer_token).await
}

async fn connect_unix_socket_http_client(
    path: &Path,
    storage_dir: Option<&Path>,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    wait_for_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_http_client(path, storage_dir, bearer_token).await
}

fn resolve_target_credential(
    target: &ServerTarget,
    storage_dir: Option<&Path>,
    allow_local_dev_token_fallback: bool,
) -> Result<Option<Credential>> {
    if let Some(token) = std::env::var("FABRO_DEV_TOKEN")
        .ok()
        .filter(|token| validate_dev_token_format(token))
    {
        return Ok(Some(Credential::DevToken(token)));
    }

    let store = AuthStore::default();
    if let Some(entry) = store.get(target)? {
        let now = chrono::Utc::now();
        if entry.access_token_expires_at > now || entry.refresh_token_expires_at > now {
            return Ok(Some(Credential::OAuth(entry)));
        }
    }

    if allow_local_dev_token_fallback {
        return Ok(load_dev_token_if_available(storage_dir).map(Credential::DevToken));
    }

    Ok(None)
}

async fn check_server_ready(http_client: &fabro_http::HttpClient) -> Result<()> {
    match http_client.get("http://fabro/health").send().await {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => bail!("server health check returned status {}", response.status()),
        Err(err) => Err(anyhow!(err)),
    }
}

async fn wait_for_server_ready(http_client: &fabro_http::HttpClient) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_error = None;

    while std::time::Instant::now() < deadline {
        match check_server_ready(http_client).await {
            Ok(()) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
        sleep(Duration::from_millis(50)).await;
    }

    Err(last_error.unwrap_or_else(|| anyhow!("server did not become ready in time")))
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "server-client tests stage local dev-token fixtures with sync std::fs::write"
)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn load_dev_token_if_available_prefers_env() {
        let temp_home = tempfile::tempdir().unwrap();
        let token_path = temp_home.path().join("dev-token");
        std::fs::write(
            &token_path,
            "fabro_dev_abababababababababababababababababababababababababababababababab",
        )
        .unwrap();

        let token = load_dev_token_if_available_from_sources(
            None,
            Some("fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
            &Home::new(temp_home.path()),
        );

        assert_eq!(
            token.as_deref(),
            Some("fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")
        );
    }

    #[test]
    fn load_dev_token_if_available_reads_file() {
        let temp_home = tempfile::tempdir().unwrap();
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        std::fs::write(temp_home.path().join("dev-token"), token).unwrap();

        let loaded =
            load_dev_token_if_available_from_sources(None, None, &Home::new(temp_home.path()));

        assert_eq!(loaded.as_deref(), Some(token));
    }

    #[test]
    fn load_dev_token_if_available_reads_path_from_active_server_record() {
        let temp_home = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let token_dir = tempfile::tempdir().unwrap();
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        let token_path = token_dir.path().join("dev-token");
        std::fs::write(&token_path, token).unwrap();

        let record_path = fabro_config::Storage::new(storage.path())
            .server_state()
            .record_path();
        record::write_server_record(&record_path, &record::ServerRecord {
            pid:            std::process::id(),
            bind:           Bind::Unix(temp_home.path().join("fabro.sock")),
            log_path:       fabro_config::Storage::new(storage.path())
                .server_state()
                .log_path(),
            dev_token_path: Some(token_path),
            started_at:     Utc::now(),
        })
        .unwrap();

        let loaded = load_dev_token_if_available_from_sources(
            Some(storage.path()),
            None,
            &Home::new(temp_home.path()),
        );

        assert_eq!(loaded.as_deref(), Some(token));
    }

    #[test]
    fn explicit_http_targets_do_not_allow_local_dev_token_fallback() {
        let target = ServerTarget::http_url("https://fabro.example.com/api/v1").unwrap();
        assert!(!local_dev_token_fallback(&target));
    }

    #[test]
    fn unix_socket_targets_keep_local_dev_token_fallback() {
        let target = ServerTarget::unix_socket_path("/tmp/fabro.sock").unwrap();
        assert!(local_dev_token_fallback(&target));
    }
}
