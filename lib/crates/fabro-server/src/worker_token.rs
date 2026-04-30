use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, Path};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use fabro_types::{CommandOutputStream, RunBlobId, RunId, StageId};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::ApiError;
use crate::auth::{self, KeyDeriveError};
use crate::jwt_auth::{authenticate_service_parts, bearer_token};
use crate::server::{AppState, parse_blob_id_path, parse_run_id_path, parse_stage_id_path};

pub(crate) const WORKER_TOKEN_ISSUER: &str = "fabro-server-worker";
pub(crate) const WORKER_TOKEN_SCOPE: &str = "run:worker";
pub(crate) const WORKER_TOKEN_TTL_SECS: u64 = 72 * 60 * 60;

#[derive(Clone)]
pub(crate) struct WorkerTokenKeys {
    encoding:   Arc<EncodingKey>,
    decoding:   Arc<DecodingKey>,
    validation: Arc<Validation>,
}

impl WorkerTokenKeys {
    pub(crate) fn from_master_secret(secret: &[u8]) -> Result<Self, KeyDeriveError> {
        let key = auth::derive_worker_jwt_key(secret)?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_nbf = false;
        validation.set_required_spec_claims(&["iss", "iat", "exp"]);
        validation.set_issuer(&[WORKER_TOKEN_ISSUER]);

        Ok(Self {
            encoding:   Arc::new(EncodingKey::from_secret(&key)),
            decoding:   Arc::new(DecodingKey::from_secret(&key)),
            validation: Arc::new(validation),
        })
    }

    #[cfg(test)]
    pub(crate) fn decoding_key(&self) -> &DecodingKey {
        &self.decoding
    }

    #[cfg(test)]
    pub(crate) fn validation(&self) -> &Validation {
        &self.validation
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct WorkerTokenClaims {
    pub(crate) iss:    String,
    pub(crate) iat:    u64,
    pub(crate) exp:    u64,
    pub(crate) run_id: String,
    pub(crate) scope:  String,
    pub(crate) jti:    String,
}

pub(crate) fn issue_worker_token(
    keys: &WorkerTokenKeys,
    run_id: &RunId,
) -> Result<String, ApiError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let claims = WorkerTokenClaims {
        iss:    WORKER_TOKEN_ISSUER.to_string(),
        iat:    now,
        exp:    now + WORKER_TOKEN_TTL_SECS,
        run_id: run_id.to_string(),
        scope:  WORKER_TOKEN_SCOPE.to_string(),
        jti:    Uuid::new_v4().simple().to_string(),
    };
    jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &keys.encoding).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to sign worker token: {err}"),
        )
    })
}

pub(crate) fn authorize_worker_token(
    parts: &Parts,
    run_id: &RunId,
    keys: &WorkerTokenKeys,
) -> Result<bool, ApiError> {
    let Some(Ok(token)) = bearer_token(parts) else {
        return Ok(false);
    };

    let claims =
        match jsonwebtoken::decode::<WorkerTokenClaims>(token, &keys.decoding, &keys.validation) {
            Ok(token_data) => token_data.claims,
            Err(_) => return Ok(false),
        };

    if claims.scope != WORKER_TOKEN_SCOPE {
        warn!(
            target: "worker_auth",
            run_id = %run_id,
            jti = %claims.jti,
            reason = "wrong_scope",
            "worker token rejected"
        );
        return Err(ApiError::forbidden());
    }
    if claims.run_id != run_id.to_string() {
        warn!(
            target: "worker_auth",
            run_id = %run_id,
            token_run_id = %claims.run_id,
            jti = %claims.jti,
            reason = "run_id_mismatch",
            "worker token rejected"
        );
        return Err(ApiError::forbidden());
    }

    debug!(
        target: "worker_auth",
        run_id = %run_id,
        jti = %claims.jti,
        "worker token accepted"
    );
    Ok(true)
}

fn authorize_run_scoped(parts: &Parts, state: &AppState, run_id: &RunId) -> Result<(), ApiError> {
    if authorize_worker_token(parts, run_id, state.worker_token_keys())? {
        return Ok(());
    }
    authenticate_service_parts(parts)
}

pub(crate) struct AuthorizeRunScoped(pub(crate) RunId);

impl FromRequestParts<Arc<AppState>> for AuthorizeRunScoped {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path(id): Path<String> = Path::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        authorize_run_scoped(parts, state.as_ref(), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id))
    }
}

pub(crate) struct AuthorizeRunBlob(pub(crate) RunId, pub(crate) RunBlobId);

impl FromRequestParts<Arc<AppState>> for AuthorizeRunBlob {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path((id, blob_id)): Path<(String, String)> = Path::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        let blob_id = parse_blob_id_path(&blob_id)?;
        authorize_run_scoped(parts, state.as_ref(), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, blob_id))
    }
}

pub(crate) struct AuthorizeStageArtifact(pub(crate) RunId, pub(crate) StageId);

impl FromRequestParts<Arc<AppState>> for AuthorizeStageArtifact {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path((id, stage_id)): Path<(String, String)> = Path::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        let stage_id = parse_stage_id_path(&stage_id)?;
        authorize_run_scoped(parts, state.as_ref(), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, stage_id))
    }
}

pub(crate) struct AuthorizeCommandLog(
    pub(crate) RunId,
    pub(crate) StageId,
    pub(crate) CommandOutputStream,
);

impl FromRequestParts<Arc<AppState>> for AuthorizeCommandLog {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path((id, stage_id, stream)): Path<(String, String, String)> =
            Path::from_request_parts(parts, state)
                .await
                .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        let stage_id = parse_stage_id_path(&stage_id)?;
        let stream = stream
            .parse::<CommandOutputStream>()
            .map_err(|_| ApiError::bad_request("Invalid command log stream.").into_response())?;
        authorize_run_scoped(parts, state.as_ref(), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, stage_id, stream))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::http::header;
    use axum::http::request::Parts;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use chrono::Duration as ChronoDuration;
    use jsonwebtoken::{Algorithm, Header, decode};
    use serde_json::json;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber, subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};
    use uuid::Uuid;

    use super::{
        WORKER_TOKEN_ISSUER, WORKER_TOKEN_SCOPE, WorkerTokenClaims, WorkerTokenKeys,
        authorize_worker_token, issue_worker_token,
    };
    use crate::auth;

    const TEST_SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const OTHER_SECRET: &[u8] = b"fedcba9876543210fedcba9876543210";

    fn keys(secret: &[u8]) -> WorkerTokenKeys {
        WorkerTokenKeys::from_master_secret(secret).expect("worker keys should derive")
    }

    fn run_id() -> fabro_types::RunId {
        "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
    }

    fn other_run_id() -> fabro_types::RunId {
        "01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap()
    }

    fn request_parts(authorization: Option<&str>) -> Parts {
        let mut builder = axum::http::Request::builder();
        if let Some(authorization) = authorization {
            builder = builder.header(header::AUTHORIZATION, authorization);
        }
        let (parts, ()) = builder.body(()).unwrap().into_parts();
        parts
    }

    fn bearer_parts(token: &str) -> Parts {
        request_parts(Some(&format!("Bearer {token}")))
    }

    fn wrong_scope_token(keys: &WorkerTokenKeys, run_id: &fabro_types::RunId) -> String {
        let claims = WorkerTokenClaims {
            iss:    WORKER_TOKEN_ISSUER.to_string(),
            iat:    1,
            exp:    u64::MAX / 2,
            run_id: run_id.to_string(),
            scope:  "wrong:scope".to_string(),
            jti:    Uuid::new_v4().simple().to_string(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &keys.encoding)
            .expect("test token should encode")
    }

    fn expired_worker_token(keys: &WorkerTokenKeys, run_id: &fabro_types::RunId) -> String {
        let claims = WorkerTokenClaims {
            iss:    WORKER_TOKEN_ISSUER.to_string(),
            iat:    1,
            exp:    2,
            run_id: run_id.to_string(),
            scope:  WORKER_TOKEN_SCOPE.to_string(),
            jti:    Uuid::new_v4().simple().to_string(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &keys.encoding)
            .expect("expired test token should encode")
    }

    fn alg_none_token(run_id: &fabro_types::RunId) -> String {
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "alg": "none",
                "typ": "JWT",
            }))
            .expect("jwt header should serialize"),
        );
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss": WORKER_TOKEN_ISSUER,
                "iat": 1_u64,
                "exp": u64::MAX / 2,
                "run_id": run_id.to_string(),
                "scope": WORKER_TOKEN_SCOPE,
                "jti": Uuid::new_v4().simple().to_string(),
            }))
            .expect("jwt payload should serialize"),
        );
        format!("{header}.{payload}.")
    }

    fn issue_user_jwt() -> String {
        let subject = auth::JwtSubject {
            identity:    fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            auth_method: fabro_types::RunAuthMethod::Github,
        };
        let key = auth::derive_jwt_key(TEST_SECRET).expect("user jwt key should derive");
        auth::issue(
            &key,
            "https://fabro.example",
            &subject,
            ChronoDuration::minutes(10),
        )
    }

    #[derive(Debug)]
    struct LogCapture {
        target: String,
        fields: Vec<(String, String)>,
    }

    #[derive(Default)]
    struct LogCaptureVisitor {
        fields: Vec<(String, String)>,
    }

    impl Visit for LogCaptureVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }

    struct LogCaptureLayer {
        events: Arc<StdMutex<Vec<LogCapture>>>,
    }

    impl<S: Subscriber> Layer<S> for LogCaptureLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            if event.metadata().target() != "worker_auth" {
                return;
            }

            let mut visitor = LogCaptureVisitor::default();
            event.record(&mut visitor);
            self.events.lock().unwrap().push(LogCapture {
                target: event.metadata().target().to_string(),
                fields: visitor.fields,
            });
        }
    }

    fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, Arc<StdMutex<Vec<LogCapture>>>) {
        let events = Arc::new(StdMutex::new(Vec::<LogCapture>::new()));
        let layer = LogCaptureLayer {
            events: Arc::clone(&events),
        };
        let subscriber = Registry::default().with(layer);
        let result = subscriber::with_default(subscriber, f);
        (result, events)
    }

    #[test]
    fn issue_worker_token_round_trips_claims() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);

        let token = issue_worker_token(&keys, &run_id).expect("worker token should issue");
        let decoded = decode::<WorkerTokenClaims>(&token, &keys.decoding, &keys.validation)
            .expect("worker token should decode");

        assert_eq!(decoded.claims, WorkerTokenClaims {
            iss:    WORKER_TOKEN_ISSUER.to_string(),
            iat:    decoded.claims.iat,
            exp:    decoded.claims.exp,
            run_id: run_id.to_string(),
            scope:  WORKER_TOKEN_SCOPE.to_string(),
            jti:    decoded.claims.jti.clone(),
        });
        assert_eq!(decoded.header.alg, Algorithm::HS256);
        assert_eq!(decoded.claims.jti.len(), 32);
    }

    #[test]
    fn worker_token_survives_key_rederivation() {
        let run_id = run_id();
        let first = keys(TEST_SECRET);
        let second = keys(TEST_SECRET);

        let token = issue_worker_token(&first, &run_id).expect("worker token should issue");
        let decoded = decode::<WorkerTokenClaims>(&token, &second.decoding, &second.validation)
            .expect("worker token should decode after re-derivation");

        assert_eq!(decoded.claims.run_id, run_id.to_string());
    }

    #[test]
    fn worker_token_fails_under_rotated_secret() {
        let run_id = run_id();
        let first = keys(TEST_SECRET);
        let second = keys(OTHER_SECRET);

        let token = issue_worker_token(&first, &run_id).expect("worker token should issue");
        let err = decode::<WorkerTokenClaims>(&token, &second.decoding, &second.validation)
            .expect_err("rotated secret should reject the token");
        assert!(matches!(
            err.kind(),
            jsonwebtoken::errors::ErrorKind::InvalidSignature
        ));
    }

    #[test]
    fn worker_key_is_distinct_from_user_jwt_key() {
        let user_key = auth::derive_jwt_key(TEST_SECRET).expect("user key should derive");
        let worker_key =
            auth::derive_worker_jwt_key(TEST_SECRET).expect("worker key should derive");

        assert_ne!(user_key.as_bytes(), worker_key);
    }

    #[test]
    fn authorize_worker_token_accepts_matching_run_id() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_worker_token(&keys, &run_id).expect("worker token should issue");
        let parts = bearer_parts(&token);

        assert!(authorize_worker_token(&parts, &run_id, &keys).unwrap());
    }

    #[test]
    fn authorize_worker_token_rejects_cross_run_reuse() {
        let run_id = run_id();
        let other_run_id = other_run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_worker_token(&keys, &other_run_id).expect("worker token should issue");
        let parts = bearer_parts(&token);

        let err = authorize_worker_token(&parts, &run_id, &keys)
            .expect_err("mismatched run should reject");
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn authorize_worker_token_rejects_wrong_scope() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = wrong_scope_token(&keys, &run_id);
        let parts = bearer_parts(&token);

        let err =
            authorize_worker_token(&parts, &run_id, &keys).expect_err("wrong scope should reject");
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn authorize_worker_token_falls_through_without_header() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let parts = request_parts(None);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        assert!(!result.unwrap());
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn authorize_worker_token_falls_through_for_user_jwt_without_worker_logs() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_user_jwt();
        let parts = bearer_parts(&token);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        assert!(!result.unwrap());
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn authorize_worker_token_falls_through_for_expired_token_without_worker_logs() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = expired_worker_token(&keys, &run_id);
        let parts = bearer_parts(&token);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        assert!(!result.unwrap());
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn authorize_worker_token_falls_through_for_bad_signature_without_worker_logs() {
        let run_id = run_id();
        let signer = keys(OTHER_SECRET);
        let verifier = keys(TEST_SECRET);
        let token = issue_worker_token(&signer, &run_id).expect("worker token should issue");
        let parts = bearer_parts(&token);

        let (result, captured) =
            capture_logs(|| authorize_worker_token(&parts, &run_id, &verifier));

        assert!(!result.unwrap());
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn authorize_worker_token_falls_through_for_alg_none_without_worker_logs() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = alg_none_token(&run_id);
        let parts = bearer_parts(&token);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        assert!(!result.unwrap());
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn authorize_worker_token_logs_acceptance() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_worker_token(&keys, &run_id).expect("worker token should issue");
        let parts = bearer_parts(&token);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        assert!(result.unwrap());
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "worker_auth");
        assert!(events[0]
            .fields
            .iter()
            .any(|(field, value)| field == "message" && value.contains("worker token accepted")));
        assert!(
            events[0]
                .fields
                .iter()
                .any(|(field, value)| field == "run_id" && value.contains(&run_id.to_string()))
        );
        assert!(
            events[0]
                .fields
                .iter()
                .any(|(field, value)| field == "jti" && !value.is_empty())
        );
    }

    #[test]
    fn authorize_worker_token_logs_run_id_mismatch() {
        let run_id = run_id();
        let other_run_id = other_run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_worker_token(&keys, &other_run_id).expect("worker token should issue");
        let parts = bearer_parts(&token);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        let err = result.expect_err("mismatched run should reject");
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "worker_auth");
        assert!(
            events[0]
                .fields
                .iter()
                .any(|(field, value)| field == "reason" && value.contains("run_id_mismatch"))
        );
    }

    #[test]
    fn authorize_worker_token_logs_wrong_scope() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = wrong_scope_token(&keys, &run_id);
        let parts = bearer_parts(&token);

        let (result, captured) = capture_logs(|| authorize_worker_token(&parts, &run_id, &keys));

        let err = result.expect_err("wrong scope should reject");
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "worker_auth");
        assert!(
            events[0]
                .fields
                .iter()
                .any(|(field, value)| field == "reason" && value.contains("wrong_scope"))
        );
    }
}
