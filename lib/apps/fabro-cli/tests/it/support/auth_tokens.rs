use std::path::Path;

use chrono::{Duration as ChronoDuration, Utc};
use fabro_config::{Storage, envfile};
use fabro_types::AuthMethod;
use hkdf::Hkdf;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use sha2::Sha256;
use ulid::Ulid;

pub(crate) const TEST_SESSION_SECRET: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const JWT_AUDIENCE: &str = "fabro-cli";
const WORKER_TOKEN_ISSUER: &str = "fabro-server-worker";
const WORKER_TOKEN_KID: &str = "fabro-worker";
const WORKER_TOKEN_SCOPE: &str = "run:worker";
const WORKER_TOKEN_TTL_SECS: u64 = 72 * 60 * 60;

#[derive(Clone)]
pub(crate) struct TestGithubJwtSubject {
    pub(crate) idp_issuer:  String,
    pub(crate) idp_subject: String,
    pub(crate) login:       String,
    pub(crate) name:        String,
    pub(crate) email:       String,
    pub(crate) avatar_url:  String,
    pub(crate) user_url:    String,
}

impl TestGithubJwtSubject {
    pub(crate) fn octocat() -> Self {
        Self {
            idp_issuer:  "https://github.com".to_string(),
            idp_subject: "12345".to_string(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
        }
    }
}

#[derive(serde::Serialize)]
struct TestJwtClaims {
    iss:         String,
    aud:         String,
    sub:         String,
    exp:         u64,
    iat:         u64,
    jti:         String,
    idp_issuer:  String,
    idp_subject: String,
    login:       String,
    name:        String,
    email:       String,
    avatar_url:  String,
    user_url:    String,
    auth_method: AuthMethod,
}

#[derive(serde::Serialize)]
struct WorkerTokenClaims {
    iss:    String,
    aud:    String,
    iat:    u64,
    exp:    u64,
    run_id: String,
    scope:  String,
    jti:    String,
}

pub(crate) fn issue_test_github_jwt(issuer: &str) -> String {
    let now = Utc::now();
    issue_github_jwt(
        issuer,
        TestGithubJwtSubject::octocat(),
        now,
        now + ChronoDuration::minutes(10),
        format!("{:032x}", rand::random::<u128>()),
    )
}

pub(crate) fn issue_expired_test_github_jwt(issuer: &str, subject: TestGithubJwtSubject) -> String {
    let now = Utc::now();
    issue_github_jwt(
        issuer,
        subject,
        now - ChronoDuration::minutes(20),
        now - ChronoDuration::minutes(10),
        Ulid::new().to_string(),
    )
}

fn issue_github_jwt(
    issuer: &str,
    subject: TestGithubJwtSubject,
    issued_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
    jti: String,
) -> String {
    let key = derived_jwt_key();
    let claims = TestJwtClaims {
        iss: issuer.to_string(),
        aud: JWT_AUDIENCE.to_string(),
        sub: subject.idp_subject.clone(),
        exp: expires_at
            .timestamp()
            .try_into()
            .expect("expiration time should be positive"),
        iat: issued_at
            .timestamp()
            .try_into()
            .expect("issued-at time should be positive"),
        jti,
        idp_issuer: subject.idp_issuer,
        idp_subject: subject.idp_subject,
        login: subject.login,
        name: subject.name,
        email: subject.email,
        avatar_url: subject.avatar_url,
        user_url: subject.user_url,
        auth_method: AuthMethod::Github,
    };
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )
    .expect("test GitHub JWT should encode")
}

fn derived_jwt_key() -> [u8; 32] {
    let hkdf = Hkdf::<Sha256>::new(None, TEST_SESSION_SECRET.as_bytes());
    let mut key = [0_u8; 32];
    hkdf.expand(b"fabro-jwt-hs256-v1", &mut key)
        .expect("HKDF should derive the fixed-size JWT key");
    key
}

#[expect(
    clippy::disallowed_methods,
    reason = "Synchronous CLI test setup reads the isolated server audience before spawning a worker."
)]
pub(crate) fn issue_test_worker_jwt(storage_dir: &Path, run_id: &str) -> String {
    let runtime_directory = Storage::new(storage_dir).runtime_directory();
    let session_secret = envfile::read_env_file(&runtime_directory.env_path())
        .expect("server env should load")
        .get("SESSION_SECRET")
        .cloned()
        .expect("server env should include SESSION_SECRET");
    let hkdf = Hkdf::<Sha256>::new(None, session_secret.as_bytes());
    let mut key = [0_u8; 32];
    hkdf.expand(b"fabro-worker-jwt-v1", &mut key)
        .expect("worker jwt hkdf output should fit");
    let now = Utc::now()
        .timestamp()
        .try_into()
        .expect("current timestamp should be positive");
    let claims = WorkerTokenClaims {
        iss:    WORKER_TOKEN_ISSUER.to_string(),
        aud:    std::fs::read_to_string(
            runtime_directory
                .record_path()
                .with_file_name("test-worker-audience"),
        )
        .expect("test server should publish its worker token audience"),
        iat:    now,
        exp:    now + WORKER_TOKEN_TTL_SECS,
        run_id: run_id.to_string(),
        scope:  WORKER_TOKEN_SCOPE.to_string(),
        jti:    format!("{:032x}", rand::random::<u128>()),
    };

    jsonwebtoken::encode(
        &worker_token_header(),
        &claims,
        &EncodingKey::from_secret(&key),
    )
    .expect("worker token should encode")
}

fn worker_token_header() -> Header {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(WORKER_TOKEN_KID.to_string());
    header
}
