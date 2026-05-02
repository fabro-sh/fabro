use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fabro_types::{CommandOutputStream, Principal, RunBlobId, RunId, StageId, UserPrincipal};
use jsonwebtoken::dangerous::insecure_decode;
use serde::Deserialize;

use crate::auth::JwtError;
use crate::error::ApiError;
use crate::jwt_auth::{self, AuthMode, ConfiguredAuth};
use crate::server::{AppState, parse_blob_id_path, parse_run_id_path, parse_stage_id_path};
use crate::worker_token::{self, WORKER_TOKEN_ISSUER};

#[derive(Clone, Debug)]
pub(crate) struct RequestAuthContext {
    pub principal:       Principal,
    pub auth_status:     AuthStatus,
    pub auth_error_code: Option<&'static str>,
    pub user_profile:    Option<UserProfile>,
}

#[derive(Clone, Debug)]
pub(crate) struct UserProfile {
    pub name:       String,
    pub email:      String,
    pub avatar_url: String,
    pub user_url:   String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthStatus {
    Missing,
    Invalid,
    Expired,
    Authenticated,
}

#[derive(Clone)]
pub(crate) struct AuthContextSlot(pub(crate) Arc<Mutex<RequestAuthContext>>);

#[allow(
    dead_code,
    reason = "Route migrations use RequestAuth first; retained for principal-only guards."
)]
pub(crate) struct RequestPrincipal(pub(crate) Principal);

pub(crate) struct RequestAuth(pub(crate) AuthContextSlot);

pub(crate) struct RequiredUser(pub(crate) UserPrincipal);
pub(crate) struct RequireRunScoped(pub(crate) RunId);
pub(crate) struct RequireRunBlob(pub(crate) RunId, pub(crate) RunBlobId);
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

#[derive(Debug, Deserialize)]
struct IssuerOnlyClaims {
    iss: String,
}

impl RequestAuthContext {
    #[must_use]
    pub(crate) fn initial() -> Self {
        Self {
            principal:       Principal::anonymous(),
            auth_status:     AuthStatus::Missing,
            auth_error_code: None,
            user_profile:    None,
        }
    }
}

impl AuthStatus {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Authenticated => "authenticated",
        }
    }
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
}

impl<S: Send + Sync> FromRequestParts<S> for RequestPrincipal {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let slot = parts
            .extensions
            .get::<AuthContextSlot>()
            .cloned()
            .unwrap_or_else(AuthContextSlot::initial);
        Ok(Self(slot.snapshot().principal))
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
        require_worker_or_user_for_run(&principal_from_parts(parts), &run_id)
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
        require_worker_or_user_for_run(&principal_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, blob_id))
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
        require_worker_or_user_for_run(&principal_from_parts(parts), &run_id)
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
        require_worker_or_user_for_run(&principal_from_parts(parts), &run_id)
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

fn principal_from_parts(parts: &Parts) -> Principal {
    parts
        .extensions
        .get::<AuthContextSlot>()
        .map_or_else(Principal::anonymous, |slot| slot.snapshot().principal)
}

pub(crate) fn require_user(slot: &AuthContextSlot) -> Result<UserPrincipal, ApiError> {
    let context = slot.snapshot();
    match context.principal {
        Principal::User(user) => Ok(user),
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

#[allow(
    dead_code,
    reason = "Worker-only route migration is staged behind the shared context."
)]
pub(crate) fn require_worker_for_run(
    principal: &Principal,
    route_run_id: &RunId,
) -> Result<(), ApiError> {
    match principal {
        Principal::Worker { run_id } if run_id == route_run_id => Ok(()),
        Principal::Worker { .. } => Err(ApiError::forbidden()),
        _ => Err(ApiError::unauthorized()),
    }
}

#[allow(
    dead_code,
    reason = "Run-scoped route migration is staged behind the shared context."
)]
pub(crate) fn require_worker_or_user_for_run(
    principal: &Principal,
    route_run_id: &RunId,
) -> Result<(), ApiError> {
    match principal {
        Principal::User(_) => Ok(()),
        Principal::Worker { run_id } if run_id == route_run_id => Ok(()),
        Principal::Worker { .. } => Err(ApiError::forbidden()),
        _ => Err(ApiError::unauthorized()),
    }
}

#[allow(
    dead_code,
    reason = "Webhook route stamps the slot inline; guard is for future webhook consumers."
)]
pub(crate) fn require_webhook(principal: &Principal) -> Result<String, ApiError> {
    match principal {
        Principal::Webhook { delivery_id } => Ok(delivery_id.clone()),
        _ => Err(ApiError::unauthorized()),
    }
}

fn classify_request(req: &Request, state: &AppState) -> RequestAuthContext {
    let AuthMode::Enabled(config) = req
        .extensions()
        .get::<AuthMode>()
        .expect("AuthMode extension must be added to the router");

    let token = match bearer_from_headers(req.headers()) {
        BearerCredential::Missing => return RequestAuthContext::initial(),
        BearerCredential::Invalid => {
            return rejected(AuthStatus::Invalid, Some("unauthorized"));
        }
        BearerCredential::Present(token) => token,
    };

    if token.starts_with("fabro_refresh_") {
        return rejected(AuthStatus::Invalid, Some("unauthorized"));
    }
    if !jwt_auth::looks_like_jwt(token) {
        return rejected(AuthStatus::Invalid, Some("access_token_invalid"));
    }

    let issuer = match insecure_decode::<IssuerOnlyClaims>(token) {
        Ok(data) => data.claims.iss,
        Err(_) => return rejected(AuthStatus::Invalid, Some("access_token_invalid")),
    };

    if issuer == WORKER_TOKEN_ISSUER {
        return match worker_token::decode_worker_token(token, state.worker_token_keys()) {
            Ok(run_id) => authenticated(Principal::worker(run_id), None),
            Err(JwtError::AccessTokenExpired) => {
                rejected(AuthStatus::Expired, Some("access_token_expired"))
            }
            Err(JwtError::AccessTokenInvalid) => {
                rejected(AuthStatus::Invalid, Some("access_token_invalid"))
            }
        };
    }

    classify_user_token(token, config)
}

enum BearerCredential<'a> {
    Missing,
    Invalid,
    Present(&'a str),
}

fn bearer_from_headers(headers: &HeaderMap) -> BearerCredential<'_> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return BearerCredential::Missing;
    };
    let Ok(value) = value.to_str() else {
        return BearerCredential::Invalid;
    };
    match value.strip_prefix("Bearer ") {
        Some(token) => BearerCredential::Present(token),
        None => BearerCredential::Invalid,
    }
}

fn classify_user_token(token: &str, config: &ConfiguredAuth) -> RequestAuthContext {
    let auth = match jwt_auth::authenticate_jwt_bearer(token, config) {
        Ok(auth) => auth,
        Err(err) if err.code() == Some("access_token_expired") => {
            return rejected(AuthStatus::Expired, Some("access_token_expired"));
        }
        Err(err) if err.code() == Some("access_token_invalid") => {
            return rejected(AuthStatus::Invalid, Some("access_token_invalid"));
        }
        Err(_) => return rejected(AuthStatus::Invalid, Some("unauthorized")),
    };
    let Some(identity) = auth.identity else {
        return rejected(AuthStatus::Invalid, Some("access_token_invalid"));
    };
    let principal = Principal::user(identity, auth.login, auth.auth_method);
    let profile = UserProfile {
        name:       auth.name,
        email:      auth.email,
        avatar_url: auth.avatar_url,
        user_url:   auth.user_url,
    };
    authenticated(principal, Some(profile))
}

fn authenticated(principal: Principal, user_profile: Option<UserProfile>) -> RequestAuthContext {
    RequestAuthContext {
        principal,
        auth_status: AuthStatus::Authenticated,
        auth_error_code: None,
        user_profile,
    }
}

fn rejected(status: AuthStatus, code: Option<&'static str>) -> RequestAuthContext {
    RequestAuthContext {
        principal:       Principal::anonymous(),
        auth_status:     status,
        auth_error_code: code,
        user_profile:    None,
    }
}

fn auth_rejection(status: AuthStatus, code: Option<&'static str>) -> ApiError {
    match (status, code) {
        (AuthStatus::Expired | AuthStatus::Invalid, Some(code)) => {
            ApiError::unauthorized_with_code("Authentication required.", code)
        }
        _ => ApiError::unauthorized(),
    }
}
