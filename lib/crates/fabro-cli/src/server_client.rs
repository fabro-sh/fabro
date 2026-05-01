use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_client::{
    AuthEntry, AuthStore, Credential, OAuthSession, ServerTarget, TransportConnector,
    apply_bearer_token_auth,
};
pub(crate) use fabro_client::{Client, RunEventStream};
use fabro_config::Storage;
use fabro_config::bind::Bind;
pub(crate) use fabro_types::RunProjection;
use fabro_types::UserSettings;
use fabro_util::dev_token;
use tokio::time::sleep;

use crate::args::ServerTargetArgs;
use crate::commands::server::start;
use crate::user_config::{self, cli_http_client_builder};

fn refreshable_oauth(
    target: &ServerTarget,
    credential: Option<&Credential>,
) -> Option<OAuthSession> {
    if matches!(credential, Some(Credential::OAuth(_))) {
        return Some(OAuthSession::new(target.clone(), AuthStore::default()));
    }
    None
}

pub(crate) async fn connect_server(storage_dir: &Path) -> Result<Client> {
    connect_local_api_client_bundle(storage_dir, &user_config::active_settings_path(None)).await
}

pub(crate) async fn connect_server_target(target: &ServerTarget) -> Result<Client> {
    connect_target_api_client_bundle(target).await
}

pub(crate) async fn connect_server_target_with_bearer(
    target: &ServerTarget,
    bearer: &str,
) -> Result<Client> {
    build_client(
        target.clone(),
        Some(Credential::Worker(bearer.to_owned())),
        None,
        None,
    )
    .await
}

pub(crate) async fn connect_server_with_settings(
    args: &ServerTargetArgs,
    settings: &UserSettings,
    storage_dir: &Path,
    base_config_path: &Path,
) -> Result<Client> {
    if let Some(target) = user_config::resolve_nondefault_server_target(args, settings)? {
        if let Some(path) = target.as_unix_socket_path() {
            return connect_managed_unix_socket_api_client_bundle(
                path,
                storage_dir,
                base_config_path,
            )
            .await;
        }
        return connect_target_api_client_bundle(&target).await;
    }

    connect_local_api_client_bundle(storage_dir, base_config_path).await
}

async fn connect_managed_unix_socket_api_client_bundle(
    path: &Path,
    storage_dir: &Path,
    active_config_path: &Path,
) -> Result<Client> {
    let target = ServerTarget::unix_socket_path(path)?;
    let runtime_token_path = Storage::new(storage_dir)
        .runtime_directory()
        .dev_token_path();
    let pre_spawn_credential = resolve_target_credential(&target)?
        .or_else(|| dev_token::read_dev_token_file(&runtime_token_path).map(Credential::DevToken));
    let pre_spawn_bearer = pre_spawn_credential.as_ref().map(Credential::bearer_token);

    let (http_client, credential) = if let Ok(http_client) =
        try_connect_unix_socket_http_client(path, pre_spawn_bearer).await
    {
        (http_client, pre_spawn_credential)
    } else {
        start::ensure_server_running_on_socket(path, active_config_path, storage_dir)
            .await
            .with_context(|| format!("Failed to start fabro server for {}", path.display()))?;
        let post_spawn_credential = match resolve_target_credential(&target)? {
            Some(credential) => Some(credential),
            None => Some(Credential::DevToken(
                wait_for_runtime_dev_token(&runtime_token_path).await?,
            )),
        };
        let post_spawn_bearer = post_spawn_credential.as_ref().map(Credential::bearer_token);
        let http_client = connect_unix_socket_http_client(path, post_spawn_bearer)
            .await
            .with_context(|| format!("Failed to connect to fabro server at {}", path.display()))?;
        (http_client, post_spawn_credential)
    };
    let oauth_session = refreshable_oauth(&target, credential.as_ref());

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
            let runtime_token_path = Storage::new(storage_dir)
                .runtime_directory()
                .dev_token_path();
            let token = wait_for_runtime_dev_token(&runtime_token_path).await?;
            let http_client = connect_unix_socket_http_client(&path, Some(&token)).await?;
            Ok(Client::from_http_client("http://fabro", http_client))
        }
        Bind::Tcp(addr) => {
            let target = ServerTarget::http_url(format!("http://{addr}"))?;
            let credential = resolve_target_credential(&target)?;
            let oauth_session = refreshable_oauth(&target, credential.as_ref());
            build_client(target, credential, oauth_session, None).await
        }
    }
}

async fn connect_target_api_client_bundle(target: &ServerTarget) -> Result<Client> {
    let credential = resolve_target_credential(target)?;
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

async fn wait_for_runtime_dev_token(path: &Path) -> Result<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while std::time::Instant::now() < deadline {
        if let Some(token) = dev_token::read_dev_token_file(path) {
            return Ok(token);
        }
        sleep(Duration::from_millis(50)).await;
    }

    bail!(
        "runtime dev token did not become available at {}",
        path.display()
    );
}

fn build_authed_unix_socket_http_client(
    path: &Path,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    let builder = cli_http_client_builder().unix_socket(path).no_proxy();
    let builder = if let Some(token) = bearer_token {
        apply_bearer_token_auth(builder, token)?
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
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    check_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_http_client(path, bearer_token)
}

async fn connect_unix_socket_http_client(
    path: &Path,
    bearer_token: Option<&str>,
) -> Result<fabro_http::HttpClient> {
    wait_for_server_ready(&build_unix_socket_probe_client(path)?).await?;
    build_authed_unix_socket_http_client(path, bearer_token)
}

fn resolve_target_credential_with_store(
    target: &ServerTarget,
    store: &AuthStore,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Credential>> {
    let Some(entry) = store.get(target)? else {
        return Ok(None);
    };
    match entry {
        AuthEntry::DevToken(entry) => Ok(Some(Credential::DevToken(entry.token))),
        AuthEntry::OAuth(entry)
            if entry.access_token_expires_at > now || entry.refresh_token_expires_at > now =>
        {
            Ok(Some(Credential::OAuth(entry)))
        }
        AuthEntry::OAuth(_) => Ok(None),
    }
}

fn resolve_target_credential(target: &ServerTarget) -> Result<Option<Credential>> {
    let store = AuthStore::default();
    resolve_target_credential_with_store(target, &store, chrono::Utc::now())
}

#[expect(
    clippy::disallowed_types,
    reason = "Proxy bypass classification parses a configured raw API target and does not log credential-bearing URLs."
)]
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
mod tests {
    use chrono::{Duration as ChronoDuration, Utc};
    use fabro_client::{AuthEntry, DevTokenEntry, OAuthEntry, StoredSubject};
    use httpmock::Method::{GET, POST};
    use serde_json::json;

    use super::*;

    #[test]
    fn resolve_target_credential_uses_persisted_dev_token_entry() {
        let dir = tempfile::tempdir().unwrap();
        let target = ServerTarget::http_url("http://127.0.0.1:32276").unwrap();
        let store = AuthStore::new(dir.path().join("auth.json"));
        let token = "fabro_dev_abababababababababababababababababababababababababababababababab";
        store
            .put(
                &target,
                AuthEntry::DevToken(DevTokenEntry {
                    token:        token.to_string(),
                    logged_in_at: Utc::now(),
                }),
            )
            .unwrap();

        let credential = resolve_target_credential_with_store(&target, &store, Utc::now()).unwrap();

        assert!(matches!(credential, Some(Credential::DevToken(found)) if found == token));
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

        let credential = resolve_target_credential_with_store(&target, &store, now).unwrap();

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

        let credential = resolve_target_credential_with_store(&target, &store, now).unwrap();

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

        let credential = resolve_target_credential_with_store(&target, &store, now).unwrap();

        assert!(credential.is_none());
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

    #[tokio::test]
    async fn connect_server_target_with_bearer_sends_worker_bearer_token() {
        let server = httpmock::MockServer::start();
        let info_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/system/info")
                .header("authorization", "Bearer worker-token");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "version": "1.2.3",
                    "git_sha": "abcdef0",
                    "build_date": "2026-04-20",
                    "profile": "release",
                    "os": "darwin",
                    "arch": "arm64",
                    "storage_dir": "/tmp/fabro-worker-auth",
                    "storage_engine": "slatedb",
                    "runs": { "total": 0, "active": 0 },
                    "uptime_secs": 42
                }));
        });

        let target = ServerTarget::http_url(server.base_url()).unwrap();
        let client = connect_server_target_with_bearer(&target, "worker-token")
            .await
            .unwrap();
        let info = client.get_system_info().await.unwrap();

        assert_eq!(info.version.as_deref(), Some("1.2.3"));
        info_mock.assert();
    }

    #[tokio::test]
    async fn connect_server_target_with_bearer_does_not_attempt_oauth_refresh() {
        let server = httpmock::MockServer::start();
        let info_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/system/info")
                .header("authorization", "Bearer worker-token");
            then.status(401)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "errors": [{
                        "status": "401",
                        "title": "Unauthorized",
                        "detail": "Access token expired.",
                        "code": "access_token_expired"
                    }]
                }));
        });
        let refresh_mock = server.mock(|when, then| {
            when.method(POST).path("/auth/cli/refresh");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "access_token": "unused",
                    "access_token_expires_at": (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
                    "refresh_token": "unused",
                    "refresh_token_expires_at": (Utc::now() + ChronoDuration::days(30)).to_rfc3339(),
                    "subject": {
                        "idp_issuer": "https://github.com",
                        "idp_subject": "12345",
                        "login": "octocat",
                        "name": "Octo Cat",
                        "email": "octocat@example.com"
                    }
                }));
        });

        let target = ServerTarget::http_url(server.base_url()).unwrap();
        let client = connect_server_target_with_bearer(&target, "worker-token")
            .await
            .unwrap();
        let err = client.get_system_info().await.unwrap_err();

        assert!(err.to_string().contains("Access token expired"));
        info_mock.assert();
        assert_eq!(refresh_mock.calls(), 0);
    }

    fn oauth_entry(
        access_token_expires_at: chrono::DateTime<chrono::Utc>,
        refresh_token_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> AuthEntry {
        AuthEntry::OAuth(OAuthEntry {
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
        })
    }
}
