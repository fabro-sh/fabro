use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use fabro_types::RunId;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use tracing::warn;
use uuid::Uuid;

use crate::ApiError;
use crate::auth::{self, JwtError, KeyDeriveError};

pub(crate) const WORKER_TOKEN_ISSUER: &str = "fabro-server-worker";
pub(crate) const WORKER_TOKEN_KID: &str = "fabro-worker";
pub(crate) const WORKER_TOKEN_SCOPE: &str = "run:worker";
pub(crate) const WORKER_RUN_TOOLS_SCOPE: &str = "agent:run_tools";
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkerScopeSet {
    agent_run_tools: bool,
}

impl WorkerScopeSet {
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn run_worker() -> Self {
        Self {
            agent_run_tools: false,
        }
    }

    #[must_use]
    pub(crate) const fn run_worker_with_agent_run_tools() -> Self {
        Self {
            agent_run_tools: true,
        }
    }

    #[must_use]
    pub(crate) const fn has_agent_run_tools(self) -> bool {
        self.agent_run_tools
    }

    fn claim(self) -> String {
        if self.agent_run_tools {
            [WORKER_TOKEN_SCOPE, WORKER_RUN_TOOLS_SCOPE].join(" ")
        } else {
            WORKER_TOKEN_SCOPE.to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DecodedWorkerToken {
    pub(crate) run_id: RunId,
    pub(crate) scopes: WorkerScopeSet,
}

#[cfg(test)]
pub(crate) fn issue_worker_token(
    keys: &WorkerTokenKeys,
    run_id: &RunId,
) -> Result<String, ApiError> {
    issue_worker_token_with_scopes(keys, run_id, WorkerScopeSet::run_worker())
}

pub(crate) fn issue_worker_token_with_scopes(
    keys: &WorkerTokenKeys,
    run_id: &RunId,
    scopes: WorkerScopeSet,
) -> Result<String, ApiError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let claims = WorkerTokenClaims {
        iss:    WORKER_TOKEN_ISSUER.to_string(),
        iat:    now,
        exp:    now + WORKER_TOKEN_TTL_SECS,
        run_id: run_id.to_string(),
        scope:  scopes.claim(),
        jti:    Uuid::new_v4().simple().to_string(),
    };
    jsonwebtoken::encode(&worker_token_header(), &claims, &keys.encoding).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to sign worker token: {err}"),
        )
    })
}

pub(crate) fn worker_token_header() -> Header {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(WORKER_TOKEN_KID.to_string());
    header
}

pub(crate) fn decode_worker_token(
    token: &str,
    keys: &WorkerTokenKeys,
) -> Result<DecodedWorkerToken, JwtError> {
    let claims = jsonwebtoken::decode::<WorkerTokenClaims>(token, &keys.decoding, &keys.validation)
        .map_err(|err| match err.kind() {
            ErrorKind::ExpiredSignature => JwtError::AccessTokenExpired,
            _ => JwtError::AccessTokenInvalid,
        })?
        .claims;

    let scopes = parse_worker_scopes(&claims.scope).map_err(|()| {
        warn!(
            target: "worker_auth",
            jti = %claims.jti,
            reason = "wrong_scope",
            "worker token rejected"
        );
        JwtError::AccessTokenInvalid
    })?;

    let run_id = claims
        .run_id
        .parse()
        .map_err(|_| JwtError::AccessTokenInvalid)?;
    Ok(DecodedWorkerToken { run_id, scopes })
}

fn parse_worker_scopes(scope: &str) -> Result<WorkerScopeSet, ()> {
    let mut has_run_worker = false;
    let mut has_agent_run_tools = false;
    for scope in scope.split_whitespace() {
        match scope {
            WORKER_TOKEN_SCOPE => has_run_worker = true,
            WORKER_RUN_TOOLS_SCOPE => has_agent_run_tools = true,
            _ => return Err(()),
        }
    }
    if !has_run_worker {
        return Err(());
    }
    Ok(WorkerScopeSet {
        agent_run_tools: has_agent_run_tools,
    })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{Algorithm, decode};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        WORKER_RUN_TOOLS_SCOPE, WORKER_TOKEN_ISSUER, WORKER_TOKEN_KID, WORKER_TOKEN_SCOPE,
        WorkerScopeSet, WorkerTokenClaims, WorkerTokenKeys, decode_worker_token,
        issue_worker_token, issue_worker_token_with_scopes, worker_token_header,
    };
    use crate::auth::{self, JwtError};

    const TEST_SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const OTHER_SECRET: &[u8] = b"fedcba9876543210fedcba9876543210";

    fn keys(secret: &[u8]) -> WorkerTokenKeys {
        WorkerTokenKeys::from_master_secret(secret).expect("worker keys should derive")
    }

    fn run_id() -> fabro_types::RunId {
        "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
    }

    fn wrong_scope_token(keys: &WorkerTokenKeys, run_id: &fabro_types::RunId) -> String {
        worker_token_with_scope(keys, run_id, "wrong:scope")
    }

    fn worker_token_with_scope(
        keys: &WorkerTokenKeys,
        run_id: &fabro_types::RunId,
        scope: &str,
    ) -> String {
        let claims = WorkerTokenClaims {
            iss:    WORKER_TOKEN_ISSUER.to_string(),
            iat:    1,
            exp:    u64::MAX / 2,
            run_id: run_id.to_string(),
            scope:  scope.to_string(),
            jti:    Uuid::new_v4().simple().to_string(),
        };
        jsonwebtoken::encode(&worker_token_header(), &claims, &keys.encoding)
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
        jsonwebtoken::encode(&worker_token_header(), &claims, &keys.encoding)
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
        assert_eq!(decoded.header.kid.as_deref(), Some(WORKER_TOKEN_KID));
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
    fn decode_worker_token_returns_run_id() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_worker_token(&keys, &run_id).expect("worker token should issue");
        let decoded = decode_worker_token(&token, &keys).unwrap();

        assert_eq!(decoded.run_id, run_id);
        assert!(!decoded.scopes.has_agent_run_tools());
    }

    #[test]
    fn decode_worker_token_exposes_run_tool_scope() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = issue_worker_token_with_scopes(
            &keys,
            &run_id,
            WorkerScopeSet::run_worker_with_agent_run_tools(),
        )
        .expect("worker token should issue");
        let decoded = decode_worker_token(&token, &keys).unwrap();

        assert_eq!(decoded.run_id, run_id);
        assert!(decoded.scopes.has_agent_run_tools());
    }

    #[test]
    fn decode_worker_token_accepts_space_delimited_run_tool_scope_claim() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = worker_token_with_scope(
            &keys,
            &run_id,
            &format!("{WORKER_TOKEN_SCOPE} {WORKER_RUN_TOOLS_SCOPE}"),
        );
        let decoded = decode_worker_token(&token, &keys).unwrap();

        assert_eq!(decoded.run_id, run_id);
        assert!(decoded.scopes.has_agent_run_tools());
    }

    #[test]
    fn decode_worker_token_rejects_missing_base_scope() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = worker_token_with_scope(&keys, &run_id, WORKER_RUN_TOOLS_SCOPE);

        assert_eq!(
            decode_worker_token(&token, &keys).expect_err("base worker scope should be required"),
            JwtError::AccessTokenInvalid,
        );
    }

    #[test]
    fn decode_worker_token_rejects_wrong_scope() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = wrong_scope_token(&keys, &run_id);

        assert_eq!(
            decode_worker_token(&token, &keys).expect_err("wrong scope should reject"),
            JwtError::AccessTokenInvalid,
        );
    }

    #[test]
    fn decode_worker_token_rejects_expired_tokens() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = expired_worker_token(&keys, &run_id);

        assert_eq!(
            decode_worker_token(&token, &keys).expect_err("expired token should reject"),
            JwtError::AccessTokenExpired,
        );
    }

    #[test]
    fn decode_worker_token_rejects_bad_signature() {
        let run_id = run_id();
        let signer = keys(OTHER_SECRET);
        let verifier = keys(TEST_SECRET);
        let token = issue_worker_token(&signer, &run_id).expect("worker token should issue");

        assert_eq!(
            decode_worker_token(&token, &verifier).expect_err("bad signature should reject"),
            JwtError::AccessTokenInvalid,
        );
    }

    #[test]
    fn decode_worker_token_rejects_alg_none() {
        let run_id = run_id();
        let keys = keys(TEST_SECRET);
        let token = alg_none_token(&run_id);

        assert_eq!(
            decode_worker_token(&token, &keys).expect_err("alg none should reject"),
            JwtError::AccessTokenInvalid,
        );
    }
}
