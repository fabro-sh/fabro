use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cookie::{Cookie, CookieJar, Key};
use fabro_server::ip_allowlist::IpAllowlistConfig;
use fabro_server::jwt_auth::resolve_auth_mode_with_lookup;
use fabro_server::server::{RouterOptions, build_router_with_options};
use fabro_server::test_support::{TEST_SESSION_SECRET, TestAppStateBuilder};
use fabro_server::web_auth::{SESSION_COOKIE_NAME, SessionCookie};
use fabro_store::{ArtifactStore, Database, RefreshToken};
use hkdf::Hkdf;
use object_store::memory::InMemory;
use sha2::Sha256;
use tower::ServiceExt;
use uuid::Uuid;

use crate::helpers::{response_json, response_status, settings_from_toml};

fn test_app(source: &str) -> (axum::Router, Arc<Database>) {
    let settings = settings_from_toml(source);
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(InMemory::new());
    let store = Arc::new(Database::new(
        Arc::clone(&object_store),
        "",
        Duration::from_millis(1),
        None,
    ));
    let artifact_store = ArtifactStore::new(object_store, "artifacts");
    let auth_mode =
        resolve_auth_mode_with_lookup(&settings.server_settings.server, |name| match name {
            "SESSION_SECRET" => Some(TEST_SESSION_SECRET.to_string()),
            "GITHUB_APP_CLIENT_SECRET" => Some("test-client-secret".to_string()),
            _ => None,
        })
        .expect("auth mode should resolve");
    let state = TestAppStateBuilder::new()
        .runtime_settings(settings.server_settings, settings.manifest_run_defaults)
        .max_concurrent_runs(5)
        .store_bundle(Arc::clone(&store), artifact_store)
        .server_secret_env(HashMap::from([(
            "SESSION_SECRET".to_string(),
            TEST_SESSION_SECRET.to_string(),
        )]))
        .build();
    let app = build_router_with_options(
        state,
        &auth_mode,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions::default(),
    );
    (app, store)
}

fn github_app() -> (axum::Router, Arc<Database>) {
    test_app(
        r#"
_version = 1

[server.auth]
methods = ["github"]

[server.auth.github]
allowed_usernames = ["octocat"]

[server.web]
url = "https://fabro.example"

[server.integrations.github]
client_id = "Iv1.test"
"#,
    )
}

fn github_identity() -> fabro_types::IdpIdentity {
    fabro_types::IdpIdentity::new("https://github.com", "12345")
        .expect("test GitHub identity should be valid")
}

fn other_identity() -> fabro_types::IdpIdentity {
    fabro_types::IdpIdentity::new("https://github.com", "67890")
        .expect("test alternate GitHub identity should be valid")
}

fn derive_cookie_key(master: &[u8]) -> Key {
    let hkdf = Hkdf::<Sha256>::new(None, master);
    let mut output = [0_u8; 64];
    hkdf.expand(b"fabro-cookie-v1", &mut output)
        .expect("fixed-size HKDF output should be valid");
    Key::from(&output)
}

fn session_cookie() -> String {
    let now = chrono::Utc::now();
    let session = SessionCookie {
        v:           2,
        login:       "octocat".to_string(),
        auth_method: fabro_types::AuthMethod::Github,
        identity:    github_identity(),
        name:        "The Octocat".to_string(),
        email:       "octocat@example.com".to_string(),
        avatar_url:  "https://avatars.githubusercontent.com/u/583231".to_string(),
        user_url:    "https://github.com/octocat".to_string(),
        iat:         now.timestamp(),
        exp:         (now + chrono::Duration::days(30)).timestamp(),
    };
    let key = derive_cookie_key(TEST_SESSION_SECRET.as_bytes());
    let mut jar = CookieJar::new();
    jar.private_mut(&key).add(
        Cookie::build((
            SESSION_COOKIE_NAME,
            serde_json::to_string(&session).expect("session should serialize"),
        ))
        .path("/")
        .http_only(true)
        .build(),
    );
    jar.delta()
        .next()
        .expect("session cookie should be set")
        .encoded()
        .to_string()
}

fn refresh_token(hash: [u8; 32], chain_id: Uuid) -> RefreshToken {
    let now = chrono::Utc::now();
    RefreshToken {
        token_hash: hash,
        chain_id,
        identity: github_identity(),
        login: "octocat".to_string(),
        name: "The Octocat".to_string(),
        email: "octocat@example.com".to_string(),
        avatar_url: None,
        issued_at: now - chrono::Duration::days(1),
        expires_at: now + chrono::Duration::days(30),
        last_used_at: now,
        used: false,
        user_agent: "fabro-cli/it".to_string(),
    }
}

async fn get_sessions(app: axum::Router, cookie: &str) -> serde_json::Value {
    response_json(
        app.oneshot(
            Request::builder()
                .uri("/api/v1/auth/sessions")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .expect("GET auth sessions request should build"),
        )
        .await
        .expect("GET auth sessions should respond"),
        StatusCode::OK,
        "GET /api/v1/auth/sessions",
    )
    .await
}

#[tokio::test]
async fn authenticated_browser_requests_receive_current_browser_session() {
    let (app, _store) = github_app();
    let body = get_sessions(app, &session_cookie()).await;

    let sessions = body["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "browser:current");
    assert_eq!(sessions[0]["kind"], "browser");
    assert_eq!(sessions[0]["current"], true);
    assert_eq!(sessions[0]["provider"], "github");
    assert_eq!(sessions[0]["login"], "octocat");
    assert_eq!(sessions[0]["label"], "This browser");
    assert_eq!(sessions[0]["userAgent"], serde_json::Value::Null);
    assert_eq!(sessions[0]["revocable"], false);
}

#[tokio::test]
async fn active_cli_refresh_token_chains_for_identity_appear_in_unified_list() {
    let (app, store) = github_app();
    let auth_tokens = store
        .refresh_tokens()
        .await
        .expect("refresh token store should open");
    let chain_id = Uuid::new_v4();
    auth_tokens
        .insert_refresh_token(refresh_token([1_u8; 32], chain_id))
        .await
        .expect("refresh token should insert");

    let body = get_sessions(app, &session_cookie()).await;
    let sessions = body["sessions"]
        .as_array()
        .expect("sessions should be an array");

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["id"], "browser:current");
    let cli = sessions
        .iter()
        .find(|session| session["id"] == format!("cli:{chain_id}"))
        .expect("CLI session should be present");
    assert_eq!(cli["kind"], "cli");
    assert_eq!(cli["current"], false);
    assert_eq!(cli["provider"], "github");
    assert_eq!(cli["login"], "octocat");
    assert_eq!(cli["label"], "Fabro CLI");
    assert_eq!(cli["userAgent"], "fabro-cli/it");
    assert_eq!(cli["revocable"], true);
}

#[tokio::test]
async fn inactive_and_other_identity_cli_tokens_are_excluded() {
    let (app, store) = github_app();
    let auth_tokens = store
        .refresh_tokens()
        .await
        .expect("refresh token store should open");
    let active_chain_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let active = refresh_token([1_u8; 32], active_chain_id);
    let mut expired = refresh_token([2_u8; 32], Uuid::new_v4());
    expired.expires_at = now - chrono::Duration::seconds(1);
    let mut used = refresh_token([3_u8; 32], Uuid::new_v4());
    used.used = true;
    let mut other = refresh_token([4_u8; 32], Uuid::new_v4());
    other.identity = other_identity();

    for token in [active, expired, used, other] {
        auth_tokens
            .insert_refresh_token(token)
            .await
            .expect("refresh token should insert");
    }

    let body = get_sessions(app, &session_cookie()).await;
    let session_ids = body["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .map(|session| {
            session["id"]
                .as_str()
                .expect("session id should be a string")
                .to_string()
        })
        .collect::<Vec<_>>();

    assert_eq!(session_ids, vec![
        "browser:current".to_string(),
        format!("cli:{active_chain_id}")
    ]);
}

#[tokio::test]
async fn deleting_cli_session_removes_refresh_token_chain() {
    let (app, store) = github_app();
    let auth_tokens = store
        .refresh_tokens()
        .await
        .expect("refresh token store should open");
    let chain_id = Uuid::new_v4();
    let active = refresh_token([1_u8; 32], chain_id);
    let mut used = refresh_token([2_u8; 32], chain_id);
    used.used = true;
    auth_tokens
        .insert_refresh_token(active)
        .await
        .expect("active refresh token should insert");
    auth_tokens
        .insert_refresh_token(used)
        .await
        .expect("used refresh token should insert");

    response_status(
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/sessions/cli:{chain_id}"))
                .header(header::COOKIE, session_cookie())
                .body(Body::empty())
                .expect("DELETE CLI auth session request should build"),
        )
        .await
        .expect("DELETE CLI auth session should respond"),
        StatusCode::NO_CONTENT,
        "DELETE /api/v1/auth/sessions/{id}",
    )
    .await;

    assert!(
        auth_tokens
            .find_refresh_token(&[1_u8; 32])
            .await
            .expect("active token lookup should succeed")
            .is_none()
    );
    assert!(
        auth_tokens
            .find_refresh_token(&[2_u8; 32])
            .await
            .expect("used token lookup should succeed")
            .is_none()
    );
}

#[tokio::test]
async fn deleting_current_browser_session_is_rejected() {
    let (app, _store) = github_app();

    let body = response_json(
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/auth/sessions/browser:current")
                .header(header::COOKIE, session_cookie())
                .body(Body::empty())
                .expect("DELETE browser auth session request should build"),
        )
        .await
        .expect("DELETE browser auth session should respond"),
        StatusCode::BAD_REQUEST,
        "DELETE /api/v1/auth/sessions/browser:current",
    )
    .await;

    assert_eq!(body["errors"][0]["status"], "400");
}

#[tokio::test]
async fn deleting_malformed_and_unknown_session_ids_returns_contract_errors() {
    let (app, _store) = github_app();
    let cookie = session_cookie();

    response_status(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/auth/sessions/cli:not-a-uuid")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .expect("malformed DELETE request should build"),
            )
            .await
            .expect("malformed DELETE should respond"),
        StatusCode::BAD_REQUEST,
        "DELETE /api/v1/auth/sessions/cli:not-a-uuid",
    )
    .await;

    response_status(
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/sessions/cli:{}", Uuid::new_v4()))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .expect("unknown DELETE request should build"),
        )
        .await
        .expect("unknown DELETE should respond"),
        StatusCode::NOT_FOUND,
        "DELETE /api/v1/auth/sessions/cli:{unknown}",
    )
    .await;
}

#[tokio::test]
async fn unauthenticated_session_requests_return_unauthorized() {
    let (app, _store) = github_app();

    response_status(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/sessions")
                    .body(Body::empty())
                    .expect("unauthenticated GET request should build"),
            )
            .await
            .expect("unauthenticated GET should respond"),
        StatusCode::UNAUTHORIZED,
        "GET /api/v1/auth/sessions",
    )
    .await;

    response_status(
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/sessions/cli:{}", Uuid::new_v4()))
                .body(Body::empty())
                .expect("unauthenticated DELETE request should build"),
        )
        .await
        .expect("unauthenticated DELETE should respond"),
        StatusCode::UNAUTHORIZED,
        "DELETE /api/v1/auth/sessions/{id}",
    )
    .await;
}
