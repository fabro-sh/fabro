use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use cookie::{Cookie, CookieJar, Key};
use fabro_server::jwt_auth::resolve_auth_mode_with_lookup;
use fabro_server::server::{AppState, RouterOptions, build_router_with_options};
use fabro_server::test_support::{TEST_SESSION_SECRET, TestAppStateBuilder};
use fabro_server::web_auth::{SESSION_COOKIE_NAME, SessionCookie};
use fabro_store::ArtifactStore;
use fabro_store::auth_session_store::{AuthSessionRecord, InitialRefreshToken};
use hkdf::Hkdf;
use object_store::memory::InMemory;
use sha2::Sha256;
use tower::ServiceExt;
use uuid::Uuid;

use crate::helpers::{response_json, response_status, settings_from_toml};

fn test_app(source: &str) -> (axum::Router, Arc<AppState>) {
    let settings = settings_from_toml(source);
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(InMemory::new());
    let store = Arc::new(fabro_store::test_support::test_database());
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
    let app = build_router_with_options(Arc::clone(&state), &auth_mode, RouterOptions::default());
    (app, state)
}

fn github_app() -> (axum::Router, Arc<AppState>) {
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

fn cli_session(id: Uuid, identity: fabro_types::IdpIdentity) -> AuthSessionRecord {
    let now = chrono::Utc::now();
    AuthSessionRecord {
        id,
        identity,
        login: "octocat".to_string(),
        name: "The Octocat".to_string(),
        email: "octocat@example.com".to_string(),
        avatar_url: String::new(),
        user_agent: "fabro-cli/it".to_string(),
        created_at: now - chrono::Duration::days(1),
        last_used_at: now,
    }
}

fn initial_refresh_token(hash: [u8; 32]) -> InitialRefreshToken {
    let now = chrono::Utc::now();
    InitialRefreshToken {
        token_hash: hash,
        issued_at:  now - chrono::Duration::days(1),
        expires_at: now + chrono::Duration::days(30),
    }
}

async fn seed_session(state: &AppState, session: AuthSessionRecord, token: InitialRefreshToken) {
    state
        .test_auth_session_store()
        .create_session(&session, &token)
        .await
        .expect("CLI session should insert");
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
    let (app, state) = github_app();
    let session_id = Uuid::new_v4();
    seed_session(
        &state,
        cli_session(session_id, github_identity()),
        initial_refresh_token([1_u8; 32]),
    )
    .await;

    let body = get_sessions(app, &session_cookie()).await;
    let sessions = body["sessions"]
        .as_array()
        .expect("sessions should be an array");

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["id"], "browser:current");
    let cli = sessions
        .iter()
        .find(|session| session["id"] == format!("cli:{session_id}"))
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
    let (app, state) = github_app();
    let active_session_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    let expired_id = Uuid::new_v4();
    let mut expired = initial_refresh_token([2_u8; 32]);
    expired.expires_at = now - chrono::Duration::seconds(1);

    // A rotated chain whose successor has expired is inactive even though its
    // original token remains as a live-but-spent replay marker.
    let spent_id = Uuid::new_v4();

    for (session, token) in [
        (
            cli_session(active_session_id, github_identity()),
            initial_refresh_token([1_u8; 32]),
        ),
        (cli_session(expired_id, github_identity()), expired),
        (
            cli_session(spent_id, github_identity()),
            initial_refresh_token([3_u8; 32]),
        ),
        (
            cli_session(Uuid::new_v4(), other_identity()),
            initial_refresh_token([4_u8; 32]),
        ),
    ] {
        seed_session(&state, session, token).await;
    }
    state
        .test_auth_session_store()
        .rotate(
            &[3_u8; 32],
            &[5_u8; 32],
            now - chrono::Duration::hours(1),
            "fabro-cli/it",
            now - chrono::Duration::hours(2),
        )
        .await
        .expect("rotation should succeed");

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
        format!("cli:{active_session_id}")
    ]);
}

#[tokio::test]
async fn deleting_cli_session_removes_refresh_token_chain() {
    let (app, state) = github_app();
    let session_id = Uuid::new_v4();
    seed_session(
        &state,
        cli_session(session_id, github_identity()),
        initial_refresh_token([1_u8; 32]),
    )
    .await;
    // Rotate once so the chain holds a spent token alongside its live one.
    let now = chrono::Utc::now();
    state
        .test_auth_session_store()
        .rotate(
            &[1_u8; 32],
            &[2_u8; 32],
            now + chrono::Duration::days(30),
            "fabro-cli/it",
            now,
        )
        .await
        .expect("rotation should succeed");

    response_status(
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/sessions/cli:{session_id}"))
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
        state
            .test_auth_session_store()
            .find_session_by_token_hash(&[1_u8; 32])
            .await
            .expect("active token lookup should succeed")
            .is_none()
    );
    assert!(
        state
            .test_auth_session_store()
            .find_session_by_token_hash(&[2_u8; 32])
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
