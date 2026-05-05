use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fabro_types::{CommandOutputStream, Principal, RunBlobId, RunId, StageId, UserPrincipal};
use jsonwebtoken::decode_header;
use strum::IntoStaticStr;

use crate::auth::{AuthErrorCode, JwtError, REFRESH_TOKEN_PREFIX};
use crate::error::ApiError;
use crate::jwt_auth::{self, AuthMode, ConfiguredAuth};
use crate::server::{AppState, parse_blob_id_path, parse_run_id_path, parse_stage_id_path};
use crate::worker_token::{self, WORKER_TOKEN_KID};

#[derive(Clone, Debug)]
pub(crate) struct RequestAuthContext {
    pub principal:       Principal,
    pub auth_status:     AuthStatus,
    pub auth_error_code: Option<AuthErrorCode>,
    pub user_profile:    Option<UserProfile>,
}

#[derive(Clone, Debug)]
pub(crate) struct UserProfile {
    pub name:       String,
    pub email:      String,
    pub avatar_url: String,
    pub user_url:   String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum AuthStatus {
    Missing,
    Invalid,
    Expired,
    Authenticated,
}

#[derive(Clone)]
pub(crate) struct AuthContextSlot(pub(crate) Arc<Mutex<RequestAuthContext>>);

// Route handlers intentionally use either this slot handle or a guard extractor
// such as RequiredUser/RequireRunScoped. A loose RequestPrincipal extractor
// would make it easy to read a principal without enforcing the route's guard.
pub(crate) struct RequestAuth(pub(crate) AuthContextSlot);

pub(crate) struct RequiredUser(pub(crate) UserPrincipal);
pub(crate) struct RequireRunScoped(pub(crate) RunId);
pub(crate) struct RequireRunBlob(pub(crate) RunId, pub(crate) RunBlobId);
pub(crate) struct RequireRunStageScoped(pub(crate) RunId, pub(crate) String);
pub(crate) struct RequireStageArtifact(pub(crate) RunId, pub(crate) StageId);
pub(crate) struct RequireCommandLog(
    pub(crate) RunId,
    pub(crate) StageId,
    pub(crate) CommandOutputStream,
);

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedUser {
    pub principal: UserPrincipal,
    pub profile:   UserProfile,
}

impl RequestAuthContext {
    #[must_use]
    pub(crate) fn initial() -> Self {
        Self {
            principal:       Principal::Anonymous,
            auth_status:     AuthStatus::Missing,
            auth_error_code: None,
            user_profile:    None,
        }
    }

    #[must_use]
    pub(crate) fn authenticated(principal: Principal, user_profile: Option<UserProfile>) -> Self {
        Self {
            principal,
            auth_status: AuthStatus::Authenticated,
            auth_error_code: None,
            user_profile,
        }
    }

    #[must_use]
    pub(crate) fn rejected(status: AuthStatus, code: Option<AuthErrorCode>) -> Self {
        Self {
            principal:       Principal::Anonymous,
            auth_status:     status,
            auth_error_code: code,
            user_profile:    None,
        }
    }

    #[must_use]
    pub(crate) fn invalid() -> Self {
        Self::rejected(AuthStatus::Invalid, Some(AuthErrorCode::Unauthorized))
    }
}

impl AuthStatus {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        self.into()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RequestAuthLogContext {
    pub principal:       Principal,
    pub auth_status:     AuthStatus,
    pub auth_error_code: Option<AuthErrorCode>,
}

impl AuthContextSlot {
    #[must_use]
    pub(crate) fn initial() -> Self {
        Self(Arc::new(Mutex::new(RequestAuthContext::initial())))
    }

    pub(crate) fn replace(&self, context: RequestAuthContext) {
        *self.0.lock().expect("auth context lock poisoned") = context;
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> RequestAuthContext {
        self.0.lock().expect("auth context lock poisoned").clone()
    }

    #[must_use]
    pub(crate) fn log_snapshot(&self) -> RequestAuthLogContext {
        let context = self.0.lock().expect("auth context lock poisoned");
        RequestAuthLogContext {
            principal:       context.principal.clone(),
            auth_status:     context.auth_status,
            auth_error_code: context.auth_error_code,
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestAuth {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let slot = parts
            .extensions
            .get::<AuthContextSlot>()
            .cloned()
            .unwrap_or_else(AuthContextSlot::initial);
        Ok(Self(slot))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequiredUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let slot = parts
            .extensions
            .get::<AuthContextSlot>()
            .cloned()
            .unwrap_or_else(AuthContextSlot::initial);
        require_user(&slot).map(Self)
    }
}

impl FromRequestParts<Arc<AppState>> for RequireRunScoped {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path(id): Path<String> = Path::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        require_worker_or_user_for_run(&auth_slot_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id))
    }
}

impl FromRequestParts<Arc<AppState>> for RequireRunBlob {
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
        require_worker_or_user_for_run(&auth_slot_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, blob_id))
    }
}

impl FromRequestParts<Arc<AppState>> for RequireRunStageScoped {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path((id, stage_id)): Path<(String, String)> = Path::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        require_worker_or_user_for_run(&auth_slot_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, stage_id))
    }
}

impl FromRequestParts<Arc<AppState>> for RequireStageArtifact {
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
        require_worker_or_user_for_run(&auth_slot_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, stage_id))
    }
}

impl FromRequestParts<Arc<AppState>> for RequireCommandLog {
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
        require_worker_or_user_for_run(&auth_slot_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, stage_id, stream))
    }
}

pub(crate) async fn principal_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let slot = req
        .extensions()
        .get::<AuthContextSlot>()
        .cloned()
        .unwrap_or_else(|| {
            let slot = AuthContextSlot::initial();
            req.extensions_mut().insert(slot.clone());
            slot
        });

    let context = classify_request(&req, state.as_ref());
    slot.replace(context);
    next.run(req).await
}

fn auth_slot_from_parts(parts: &Parts) -> AuthContextSlot {
    parts
        .extensions
        .get::<AuthContextSlot>()
        .cloned()
        .unwrap_or_else(AuthContextSlot::initial)
}

pub(crate) fn require_user(slot: &AuthContextSlot) -> Result<UserPrincipal, ApiError> {
    let context = slot.0.lock().expect("auth context lock poisoned");
    match &context.principal {
        Principal::User(user) => Ok(user.clone()),
        _ => Err(auth_rejection(context.auth_status, context.auth_error_code)),
    }
}

pub(crate) fn require_authenticated_user(
    slot: &AuthContextSlot,
) -> Result<AuthenticatedUser, ApiError> {
    let context = slot.snapshot();
    match context.principal {
        Principal::User(principal) => {
            let Some(profile) = context.user_profile else {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Authenticated user profile missing.",
                ));
            };
            Ok(AuthenticatedUser { principal, profile })
        }
        _ => Err(auth_rejection(context.auth_status, context.auth_error_code)),
    }
}

fn require_worker_or_user_for_run(
    slot: &AuthContextSlot,
    route_run_id: &RunId,
) -> Result<(), ApiError> {
    let context = slot.0.lock().expect("auth context lock poisoned");
    match &context.principal {
        Principal::User(_) => Ok(()),
        Principal::Worker { run_id } if run_id == route_run_id => Ok(()),
        Principal::Worker { .. } => Err(ApiError::forbidden()),
        _ => Err(auth_rejection(context.auth_status, context.auth_error_code)),
    }
}

fn classify_request(req: &Request, state: &AppState) -> RequestAuthContext {
    let AuthMode::Enabled(config) = req
        .extensions()
        .get::<AuthMode>()
        .expect("AuthMode extension must be added to the router");

    let token = match jwt_auth::bearer_token_from_headers(req.headers()) {
        None => return RequestAuthContext::initial(),
        Some(Err(_)) => {
            return RequestAuthContext::rejected(
                AuthStatus::Invalid,
                Some(AuthErrorCode::Unauthorized),
            );
        }
        Some(Ok(token)) => token,
    };

    if token.starts_with(REFRESH_TOKEN_PREFIX) {
        return RequestAuthContext::rejected(
            AuthStatus::Invalid,
            Some(AuthErrorCode::Unauthorized),
        );
    }
    if !jwt_auth::looks_like_jwt(token) {
        return RequestAuthContext::rejected(
            AuthStatus::Invalid,
            Some(AuthErrorCode::AccessTokenInvalid),
        );
    }

    let Ok(header) = decode_header(token) else {
        return RequestAuthContext::rejected(
            AuthStatus::Invalid,
            Some(AuthErrorCode::AccessTokenInvalid),
        );
    };

    if header.kid.as_deref() == Some(WORKER_TOKEN_KID) {
        return match worker_token::decode_worker_token(token, state.worker_token_keys()) {
            Ok(run_id) => RequestAuthContext::authenticated(Principal::Worker { run_id }, None),
            Err(JwtError::AccessTokenExpired) => RequestAuthContext::rejected(
                AuthStatus::Expired,
                Some(AuthErrorCode::AccessTokenExpired),
            ),
            Err(JwtError::AccessTokenInvalid) => RequestAuthContext::rejected(
                AuthStatus::Invalid,
                Some(AuthErrorCode::AccessTokenInvalid),
            ),
        };
    }

    classify_user_token(token, config)
}

fn classify_user_token(token: &str, config: &ConfiguredAuth) -> RequestAuthContext {
    let auth = match jwt_auth::authenticate_jwt_bearer(token, config) {
        Ok(auth) => auth,
        Err(err) if err.code() == Some(AuthErrorCode::AccessTokenExpired.as_str()) => {
            return RequestAuthContext::rejected(
                AuthStatus::Expired,
                Some(AuthErrorCode::AccessTokenExpired),
            );
        }
        Err(err) if err.code() == Some(AuthErrorCode::AccessTokenInvalid.as_str()) => {
            return RequestAuthContext::rejected(
                AuthStatus::Invalid,
                Some(AuthErrorCode::AccessTokenInvalid),
            );
        }
        Err(_) => {
            return RequestAuthContext::rejected(
                AuthStatus::Invalid,
                Some(AuthErrorCode::Unauthorized),
            );
        }
    };
    let principal = Principal::user(auth.identity, auth.login, auth.auth_method);
    let profile = UserProfile {
        name:       auth.name,
        email:      auth.email,
        avatar_url: auth.avatar_url,
        user_url:   auth.user_url,
    };
    RequestAuthContext::authenticated(principal, Some(profile))
}

fn auth_rejection(status: AuthStatus, code: Option<AuthErrorCode>) -> ApiError {
    match (status, code) {
        (AuthStatus::Expired | AuthStatus::Invalid, Some(code)) => {
            ApiError::unauthorized_with_code("Authentication required.", code.as_str())
        }
        _ => ApiError::unauthorized(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, header};
    use chrono::Duration;
    use fabro_static::EnvVars;
    use fabro_types::settings::ServerAuthMethod;
    use fabro_types::{AuthMethod, IdpIdentity, RunId};
    use jsonwebtoken::EncodingKey;
    use uuid::Uuid;

    use super::*;
    use crate::auth::{self, AuthErrorCode};
    use crate::worker_token::{
        WORKER_TOKEN_ISSUER, WORKER_TOKEN_SCOPE, WorkerTokenClaims, issue_worker_token,
        worker_token_header,
    };

    const TEST_JWT_ISSUER: &str = "https://fabro.example";

    fn auth_mode_for_state(state: &AppState) -> AuthMode {
        let secret = state
            .server_secret(EnvVars::SESSION_SECRET)
            .expect("test state should have session secret");
        AuthMode::Enabled(ConfiguredAuth {
            methods:    vec![ServerAuthMethod::Github],
            dev_token:  None,
            jwt_key:    Some(auth::derive_jwt_key(secret.as_bytes()).unwrap()),
            jwt_issuer: Some(TEST_JWT_ISSUER.to_string()),
        })
    }

    fn request_with_bearer(token: Option<&str>, auth_mode: AuthMode) -> Request<Body> {
        let mut builder = Request::builder().uri("/api/v1/auth/me");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = builder.body(Body::empty()).unwrap();
        request.extensions_mut().insert(auth_mode);
        request
    }

    fn user_subject() -> auth::JwtSubject {
        auth::JwtSubject {
            identity:    IdpIdentity::new("https://github.com", "12345").unwrap(),
            login:       "octocat".to_string(),
            name:        "The Octocat".to_string(),
            email:       "octocat@example.com".to_string(),
            avatar_url:  "https://example.com/octocat.png".to_string(),
            user_url:    "https://github.com/octocat".to_string(),
            auth_method: AuthMethod::Github,
        }
    }

    fn issue_user_token(state: &AppState, ttl: Duration) -> String {
        let secret = state
            .server_secret(EnvVars::SESSION_SECRET)
            .expect("test state should have session secret");
        let key = auth::derive_jwt_key(secret.as_bytes()).unwrap();
        auth::issue(&key, TEST_JWT_ISSUER, &user_subject(), ttl)
    }

    fn issue_user_token_with_other_secret() -> String {
        let key = auth::derive_jwt_key(b"other-principal-middleware-secret-0001").unwrap();
        auth::issue(
            &key,
            TEST_JWT_ISSUER,
            &user_subject(),
            Duration::minutes(10),
        )
    }

    fn issue_worker_claims(state: &AppState, run_id: RunId, exp: u64, scope: &str) -> String {
        let secret = state
            .server_secret(EnvVars::SESSION_SECRET)
            .expect("test state should have session secret");
        issue_worker_claims_with_secret(secret.as_bytes(), run_id, exp, scope)
    }

    fn issue_worker_claims_with_secret(
        secret: &[u8],
        run_id: RunId,
        exp: u64,
        scope: &str,
    ) -> String {
        let worker_key = auth::derive_worker_jwt_key(secret).unwrap();
        let claims = WorkerTokenClaims {
            iss: WORKER_TOKEN_ISSUER.to_string(),
            iat: 1,
            exp,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            jti: Uuid::new_v4().simple().to_string(),
        };
        jsonwebtoken::encode(
            &worker_token_header(),
            &claims,
            &EncodingKey::from_secret(&worker_key),
        )
        .unwrap()
    }

    fn classify_token(token: Option<&str>) -> RequestAuthContext {
        let state = crate::test_support::test_app_state();
        let request = request_with_bearer(token, auth_mode_for_state(state.as_ref()));
        classify_request(&request, state.as_ref())
    }

    #[test]
    fn classifies_valid_user_jwt_as_user_principal() {
        let state = crate::test_support::test_app_state();
        let token = issue_user_token(state.as_ref(), Duration::minutes(10));
        let request = request_with_bearer(Some(&token), auth_mode_for_state(state.as_ref()));

        let context = classify_request(&request, state.as_ref());

        assert_eq!(context.auth_status, AuthStatus::Authenticated);
        assert!(matches!(context.principal, Principal::User(_)));
        assert!(context.user_profile.is_some());
    }

    #[test]
    fn classifies_expired_user_jwt_as_expired() {
        let state = crate::test_support::test_app_state();
        let token = issue_user_token(state.as_ref(), Duration::seconds(-60));
        let request = request_with_bearer(Some(&token), auth_mode_for_state(state.as_ref()));

        let context = classify_request(&request, state.as_ref());

        assert_eq!(context.auth_status, AuthStatus::Expired);
        assert_eq!(
            context.auth_error_code,
            Some(AuthErrorCode::AccessTokenExpired)
        );
    }

    #[test]
    fn classifies_invalid_user_jwt_signature_as_invalid() {
        let context = classify_token(Some(&issue_user_token_with_other_secret()));

        assert_eq!(context.auth_status, AuthStatus::Invalid);
        assert_eq!(
            context.auth_error_code,
            Some(AuthErrorCode::AccessTokenInvalid)
        );
    }

    #[test]
    fn routes_worker_kid_to_worker_verifier() {
        let state = crate::test_support::test_app_state();
        let run_id = RunId::new();
        let token = issue_worker_token(state.worker_token_keys(), &run_id).unwrap();
        let request = request_with_bearer(Some(&token), auth_mode_for_state(state.as_ref()));

        let context = classify_request(&request, state.as_ref());

        assert_eq!(context.auth_status, AuthStatus::Authenticated);
        assert_eq!(context.principal, Principal::Worker { run_id });
    }

    #[test]
    fn classifies_expired_worker_jwt_as_expired_not_invalid() {
        let state = crate::test_support::test_app_state();
        let token = issue_worker_claims(state.as_ref(), RunId::new(), 2, WORKER_TOKEN_SCOPE);
        let request = request_with_bearer(Some(&token), auth_mode_for_state(state.as_ref()));

        let context = classify_request(&request, state.as_ref());

        assert_eq!(context.auth_status, AuthStatus::Expired);
        assert_eq!(
            context.auth_error_code,
            Some(AuthErrorCode::AccessTokenExpired)
        );
    }

    #[test]
    fn classifies_invalid_worker_jwt_signature_as_invalid() {
        let state = crate::test_support::test_app_state();
        let token = issue_worker_claims_with_secret(
            b"other-principal-middleware-secret-0001",
            RunId::new(),
            u64::MAX / 2,
            WORKER_TOKEN_SCOPE,
        );
        let request = request_with_bearer(Some(&token), auth_mode_for_state(state.as_ref()));

        let context = classify_request(&request, state.as_ref());

        assert_eq!(context.auth_status, AuthStatus::Invalid);
        assert_eq!(
            context.auth_error_code,
            Some(AuthErrorCode::AccessTokenInvalid)
        );
    }

    #[test]
    fn classifies_wrong_scope_worker_jwt_as_invalid() {
        let state = crate::test_support::test_app_state();
        let token = issue_worker_claims(state.as_ref(), RunId::new(), u64::MAX / 2, "wrong:scope");
        let request = request_with_bearer(Some(&token), auth_mode_for_state(state.as_ref()));

        let context = classify_request(&request, state.as_ref());

        assert_eq!(context.auth_status, AuthStatus::Invalid);
        assert_eq!(
            context.auth_error_code,
            Some(AuthErrorCode::AccessTokenInvalid)
        );
    }

    #[test]
    fn classifies_refresh_token_at_protected_endpoint_as_unauthorized() {
        let context = classify_token(Some("fabro_refresh_secret"));

        assert_eq!(context.auth_status, AuthStatus::Invalid);
        assert_eq!(context.auth_error_code, Some(AuthErrorCode::Unauthorized));
    }

    #[test]
    fn classifies_malformed_bearer_as_invalid_access_token() {
        let context = classify_token(Some("not-a-jwt"));

        assert_eq!(context.auth_status, AuthStatus::Invalid);
        assert_eq!(
            context.auth_error_code,
            Some(AuthErrorCode::AccessTokenInvalid)
        );
    }

    #[test]
    fn classifies_missing_bearer_as_missing() {
        let context = classify_token(None);

        assert_eq!(context.auth_status, AuthStatus::Missing);
        assert_eq!(context.auth_error_code, None);
        assert_eq!(context.principal, Principal::Anonymous);
    }

    #[test]
    fn run_scoped_guard_preserves_expired_auth_error_code() {
        let context = RequestAuthContext::rejected(
            AuthStatus::Expired,
            Some(AuthErrorCode::AccessTokenExpired),
        );

        let slot = AuthContextSlot::initial();
        slot.replace(context);
        let err = require_worker_or_user_for_run(&slot, &RunId::new()).unwrap_err();

        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.code(), Some("access_token_expired"));
    }

    #[test]
    fn run_scoped_guard_preserves_invalid_auth_error_code() {
        let context = RequestAuthContext::rejected(
            AuthStatus::Invalid,
            Some(AuthErrorCode::AccessTokenInvalid),
        );

        let slot = AuthContextSlot::initial();
        slot.replace(context);
        let err = require_worker_or_user_for_run(&slot, &RunId::new()).unwrap_err();

        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.code(), Some("access_token_invalid"));
    }
}
