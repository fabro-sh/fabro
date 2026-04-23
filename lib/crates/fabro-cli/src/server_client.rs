use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_client::{
    AuthStore, Credential, CredentialFallback, OAuthSession, ServerTarget, TransportConnector,
    apply_bearer_token_auth,
};
pub(crate) use fabro_client::{Client, RunEventStream};
use fabro_config::bind::Bind;
pub(crate) use fabro_types::RunProjection;
use fabro_types::settings::SettingsLayer;
use fabro_util::dev_token::validate_dev_token_format;
use fabro_util::{Home, dev_token};
use tokio::time::sleep;

use crate::args::ServerTargetArgs;
use crate::commands::server::start;
use crate::local_server;
use crate::user_config::{self, cli_http_client_builder};

#[derive(Debug)]
struct CliDevTokenFallback;

impl CredentialFallback for CliDevTokenFallback {
    fn resolve(&self) -> Option<Credential> {
        load_cli_dev_token().map(Credential::DevToken)
    }
}

fn refreshable_oauth(
    target: &ServerTarget,
    credential: Option<&Credential>,
) -> Option<OAuthSession> {
    if matches!(credential, Some(Credential::OAuth(_))) {
        let session = OAuthSession::new(target.clone(), AuthStore::default());
        if local_dev_token_fallback(target) {
            return Some(session.with_fallback(Arc::new(CliDevTokenFallback)));
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
                &local_server::storage_dir(settings)?,
                base_config_path,
            )
            .await;
        }
        return connect_target_api_client_bundle(&target).await;
    }

    connect_local_api_client_bundle(&local_server::storage_dir(settings)?, base_config_path).await
}

async fn connect_managed_unix_socket_api_client_bundle(
    path: &Path,
    storage_dir: &Path,
    active_config_path: &Path,
) -> Result<Client> {
    let target = ServerTarget::unix_socket_path(path)?;
    let credential = resolve_target_credential(&target, local_dev_token_fallback(&target))?;
    let oauth_session = refreshable_oauth(&target, credential.as_ref());
    let bearer_token = credential.as_ref().map(Credential::bearer_token);

    let http_client = if let Ok(http_client) =
        try_connect_unix_socket_http_client(path, true, bearer_token).await
    {
        http_client
    } else {
        start::ensure_server_running_on_socket(path, active_config_path, storage_dir)
            .await
            .with_context(|| format!("Failed to start fabro server for {}", path.display()))?;
        connect_unix_socket_http_client(path, true, bearer_token)
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
            let http_client = connect_unix_socket_http_client(&path, true, None).await?;
            Ok(Client::from_http_client("http://fabro", http_client))
        }
        Bind::Tcp(addr) => {
            let target = ServerTarget::http_url(format!("http://{addr}"))?;
            let credential = resolve_local_tcp_credential(&target)?;
            let oauth_session = refreshable_oauth(&target, credential.as_ref());
            build_client(target, credential, oauth_session, None).await
        }
    }
}

async fn connect_target_api_client_bundle(target: &ServerTarget) -> Result<Client> {
    let credential = resolve_target_credential(target, local_dev_token_fallback(target))?;
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
        if should_bypass_proxy_for_http_target(api_url) {
            builder = builder.no_proxy();
        }
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

fn load_cli_dev_token() -> Option<String> {
    let env_token = std::env::var("FABRO_DEV_TOKEN").ok();
    load_cli_dev_token_from_sources(env_token.as_deref(), &Home::from_env())
}

fn load_cli_dev_token_from_sources(env_token: Option<&str>, home: &Home) -> Option<String> {
    if let Some(token) = env_token.filter(|token| validate_dev_token_format(token)) {
        return Some(token.to_owned());
    }

    dev_token::read_dev_token_file(&home.dev_token_path())
}

async fn wait_for_cli_dev_token() -> Result<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while std::time::Instant::now() < deadline {
        if let Some(token) = load_cli_dev_token() {
            return Ok(token);
        }
        sleep(Duration::from_millis(50)).await;
    }

    bail!("local CLI dev token did not become available");
}

async fn build_authed_unix_socket_http_client(
    path: &Path,
    wait_for_cli_dev_token_fallback: bool,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    let builder = cli_http_client_builder().unix_socket(path).no_proxy();
    let builder = if let Some(token) = bearer_token {
        apply_bearer_token_auth(builder, token)?
    } else if wait_for_cli_dev_token_fallback {
        let token = wait_for_cli_dev_token().await?;
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
    wait_for_cli_dev_token_fallback: bool,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    check_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_http_client(path, wait_for_cli_dev_token_fallback, bearer_token).await
}

async fn connect_unix_socket_http_client(
    path: &Path,
    wait_for_cli_dev_token_fallback: bool,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    wait_for_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_http_client(path, wait_for_cli_dev_token_fallback, bearer_token).await
}

fn resolve_oauth_credential(
    target: &ServerTarget,
    store: &AuthStore,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Credential>> {
    if let Some(entry) = store.get(target)? {
        if entry.access_token_expires_at > now || entry.refresh_token_expires_at > now {
            return Ok(Some(Credential::OAuth(entry)));
        }
    }

    Ok(None)
}

fn resolve_local_tcp_credential_with_store(
    target: &ServerTarget,
    env_token: Option<&str>,
    store: &AuthStore,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Credential>> {
    if let Some(token) = env_token.filter(|token| validate_dev_token_format(token)) {
        return Ok(Some(Credential::DevToken(token.to_owned())));
    }

    resolve_oauth_credential(target, store, now)
}

fn resolve_local_tcp_credential(target: &ServerTarget) -> Result<Option<Credential>> {
    let env_token = std::env::var("FABRO_DEV_TOKEN").ok();
    let store = AuthStore::default();
    resolve_local_tcp_credential_with_store(
        target,
        env_token.as_deref(),
        &store,
        chrono::Utc::now(),
    )
}

fn resolve_target_credential(
    target: &ServerTarget,
    allow_local_dev_token_fallback: bool,
) -> Result<Option<Credential>> {
    let env_token = std::env::var("FABRO_DEV_TOKEN").ok();
    let store = AuthStore::default();
    if let Some(credential) = resolve_local_tcp_credential_with_store(
        target,
        env_token.as_deref(),
        &store,
        chrono::Utc::now(),
    )? {
        return Ok(Some(credential));
    }

    if allow_local_dev_token_fallback {
        return Ok(
            load_cli_dev_token_from_sources(env_token.as_deref(), &Home::from_env())
                .map(Credential::DevToken),
        );
    }

    Ok(None)
}

fn should_bypass_proxy_for_http_target(api_url: &str) -> bool {
    let Ok(url) = fabro_http::Url::parse(api_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
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
    use chrono::{Duration as ChronoDuration, Utc};
    use fabro_client::{AuthEntry, StoredSubject};

    use super::*;

    #[test]
    fn load_cli_dev_token_prefers_env() {
        let temp_home = tempfile::tempdir().unwrap();
        let token_path = temp_home.path().join("dev-token");
        std::fs::write(
            &token_path,
            "fabro_dev_abababababababababababababababababababababababababababababababab",
        )
        .unwrap();

        let token = load_cli_dev_token_from_sources(
            Some("fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
            &Home::new(temp_home.path()),
        );

        assert_eq!(
            token.as_deref(),
            Some("fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")
        );
    }

    #[test]
    fn load_cli_dev_token_reads_home_file() {
        let temp_home = tempfile::tempdir().unwrap();
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        std::fs::write(temp_home.path().join("dev-token"), token).unwrap();

        let loaded = load_cli_dev_token_from_sources(None, &Home::new(temp_home.path()));

        assert_eq!(loaded.as_deref(), Some(token));
    }

    #[test]
    fn resolve_local_tcp_credential_prefers_valid_env_token() {
        let target = ServerTarget::http_url("http://127.0.0.1:32276").unwrap();
        let store = AuthStore::new(tempfile::tempdir().unwrap().path().join("auth.json"));

        let credential = resolve_local_tcp_credential_with_store(
            &target,
            Some("fabro_dev_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"),
            &store,
            Utc::now(),
        )
        .unwrap();

        assert!(matches!(credential, Some(Credential::DevToken(_))));
    }

    #[test]
    fn resolve_local_tcp_credential_uses_live_oauth_entry() {
        let dir = tempfile::tempdir().unwrap();
        let target = ServerTarget::http_url("http://127.0.0.1:32276").unwrap();
        let store = AuthStore::new(dir.path().join("auth.json"));
        let now = Utc::now();
        store
            .put(
                &target,
                oauth_entry(
                    now + ChronoDuration::minutes(5),
                    now - ChronoDuration::minutes(1),
                ),
            )
            .unwrap();

        let credential =
            resolve_local_tcp_credential_with_store(&target, None, &store, now).unwrap();

        assert!(matches!(credential, Some(Credential::OAuth(_))));
    }

    #[test]
    fn resolve_local_tcp_credential_uses_refreshable_oauth_entry() {
        let dir = tempfile::tempdir().unwrap();
        let target = ServerTarget::http_url("http://127.0.0.1:32276").unwrap();
        let store = AuthStore::new(dir.path().join("auth.json"));
        let now = Utc::now();
        store
            .put(
                &target,
                oauth_entry(
                    now - ChronoDuration::minutes(1),
                    now + ChronoDuration::minutes(5),
                ),
            )
            .unwrap();

        let credential =
            resolve_local_tcp_credential_with_store(&target, None, &store, now).unwrap();

        assert!(matches!(credential, Some(Credential::OAuth(_))));
    }

    #[test]
    fn resolve_local_tcp_credential_ignores_expired_oauth_entry() {
        let dir = tempfile::tempdir().unwrap();
        let target = ServerTarget::http_url("http://127.0.0.1:32276").unwrap();
        let store = AuthStore::new(dir.path().join("auth.json"));
        let now = Utc::now();
        store
            .put(
                &target,
                oauth_entry(
                    now - ChronoDuration::minutes(5),
                    now - ChronoDuration::minutes(1),
                ),
            )
            .unwrap();

        let credential =
            resolve_local_tcp_credential_with_store(&target, None, &store, now).unwrap();

        assert!(credential.is_none());
    }

    #[test]
    fn resolve_local_tcp_credential_does_not_fallback_to_home_dev_token() {
        let temp_home = tempfile::tempdir().unwrap();
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        std::fs::write(temp_home.path().join("dev-token"), token).unwrap();
        let target = ServerTarget::http_url("http://127.0.0.1:32276").unwrap();
        let store = AuthStore::new(temp_home.path().join("auth.json"));
        assert_eq!(
            load_cli_dev_token_from_sources(None, &Home::new(temp_home.path())).as_deref(),
            Some(token)
        );

        assert!(
            resolve_local_tcp_credential_with_store(&target, None, &store, Utc::now())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn bypasses_proxy_for_loopback_http_targets() {
        assert!(should_bypass_proxy_for_http_target(
            "http://127.0.0.1:32276"
        ));
        assert!(should_bypass_proxy_for_http_target("http://[::1]:32276"));
        assert!(should_bypass_proxy_for_http_target(
            "http://localhost:32276"
        ));
        assert!(!should_bypass_proxy_for_http_target(
            "https://fabro.example.com"
        ));
        assert!(!should_bypass_proxy_for_http_target(
            "http://fabro.example.com"
        ));
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

    fn oauth_entry(
        access_token_expires_at: chrono::DateTime<chrono::Utc>,
        refresh_token_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> AuthEntry {
        AuthEntry {
            access_token: "access-token".to_string(),
            access_token_expires_at,
            refresh_token: "refresh-token".to_string(),
            refresh_token_expires_at,
            subject: StoredSubject {
                idp_issuer:  "https://github.com/login/oauth".to_string(),
                idp_subject: "subject-123".to_string(),
                login:       "octocat".to_string(),
                name:        "Octo Cat".to_string(),
                email:       "octocat@example.com".to_string(),
            },
            logged_in_at: Utc::now(),
        }
    }
}
