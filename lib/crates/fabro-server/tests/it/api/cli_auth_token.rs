use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use fabro_server::ip_allowlist::IpAllowlistConfig;
use fabro_server::jwt_auth::resolve_auth_mode_with_lookup;
use fabro_server::server::{
    RouterOptions, build_router_with_options, create_app_state_with_store_and_runtime_settings,
};
use fabro_store::{ArtifactStore, AuthCode, Database, RefreshToken};
use object_store::memory::InMemory;
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

use crate::helpers::{body_json, settings_from_toml};

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
            "SESSION_SECRET" => Some("0123456789abcdef0123456789abcdef".to_string()),
            "GITHUB_APP_CLIENT_SECRET" => Some("test-client-secret".to_string()),
            _ => None,
        })
        .expect("auth mode should resolve");
    let app = build_router_with_options(
        create_app_state_with_store_and_runtime_settings(
            settings.server_settings,
            settings.manifest_run_defaults,
            5,
            Arc::clone(&store),
            artifact_store,
        ),
        &auth_mode,
        Arc::new(IpAllowlistConfig::default()),
        RouterOptions::default(),
    );
    (app, store)
}

fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn hash_refresh_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[tokio::test]
async fn cli_auth_token_exchanges_code_over_public_router() {
    let (app, store) = test_app(
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
    );
    let auth_codes = store.auth_codes().await.unwrap();
    auth_codes
        .insert(AuthCode {
            code:           "integration-code".to_string(),
            identity:       fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:          "octocat".to_string(),
            name:           "The Octocat".to_string(),
            email:          "octocat@example.com".to_string(),
            code_challenge: pkce_challenge("integration-verifier"),
            redirect_uri:   "http://127.0.0.1:4444/callback".to_string(),
            expires_at:     chrono::Utc::now() + chrono::Duration::seconds(60),
        })
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/cli/token")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::USER_AGENT, "fabro-cli/it")
                .body(Body::from(
                    serde_json::json!({
                        "grant_type": "authorization_code",
                        "code": "integration-code",
                        "code_verifier": "integration-verifier",
                        "redirect_uri": "http://127.0.0.1:4444/callback"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    assert!(body["access_token"].as_str().unwrap().starts_with("eyJ"));
    assert!(
        body["refresh_token"]
            .as_str()
            .unwrap()
            .starts_with("fabro_refresh_")
    );
    assert_eq!(body["subject"]["login"], "octocat");
}

#[tokio::test]
async fn cli_auth_refresh_replay_revokes_chain_over_public_router() {
    let (app, store) = test_app(
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
    );
    let auth_tokens = store.refresh_tokens().await.unwrap();
    let now = chrono::Utc::now();
    auth_tokens
        .insert_refresh_token(RefreshToken {
            token_hash:   hash_refresh_secret("integration-refresh"),
            chain_id:     Uuid::new_v4(),
            identity:     fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:        "octocat".to_string(),
            name:         "The Octocat".to_string(),
            email:        "octocat@example.com".to_string(),
            issued_at:    now,
            expires_at:   now + chrono::Duration::days(30),
            last_used_at: now,
            used:         false,
            user_agent:   "fabro-cli/it".to_string(),
        })
        .await
        .unwrap();

    let refresh_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/cli/refresh")
            .header(
                header::AUTHORIZATION,
                "Bearer fabro_refresh_integration-refresh",
            )
            .body(Body::empty())
            .unwrap()
    };

    let first = app.clone().oneshot(refresh_request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let replay = app.oneshot(refresh_request()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    let body = body_json(replay.into_body()).await;
    assert_eq!(body["error"], "refresh_token_revoked");
}
