use std::collections::HashMap;
use std::sync::Arc;

use axum::http::HeaderMap;
use fabro_environment::{Environment, EnvironmentDraft, EnvironmentId, EnvironmentStoreError};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
    EnvironmentNetworkSettings, EnvironmentProvider, EnvironmentResourcesSettings,
    EnvironmentSettings, EnvironmentVolumeSettings,
};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use super::super::{
    ApiError, AppState, IntoResponse, Json, Path, RequiredUser, Response, Router, State,
    StatusCode, get,
};
use super::{json_with_etag_response, parse_required_if_match};

#[derive(Serialize)]
struct EnvironmentListResponse {
    data: Vec<Environment>,
    meta: EnvironmentListMeta,
}

#[derive(Serialize)]
struct EnvironmentListMeta {
    total: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEnvironmentRequest {
    id:        EnvironmentId,
    provider:  EnvironmentProvider,
    image:     ApiEnvironmentImageSettings,
    resources: EnvironmentResourcesSettings,
    network:   EnvironmentNetworkSettings,
    lifecycle: EnvironmentLifecycleSettings,
    labels:    HashMap<String, String>,
    volumes:   Vec<EnvironmentVolumeSettings>,
    env:       HashMap<String, InterpString>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceEnvironmentRequest {
    provider:  EnvironmentProvider,
    image:     ApiEnvironmentImageSettings,
    resources: EnvironmentResourcesSettings,
    network:   EnvironmentNetworkSettings,
    lifecycle: EnvironmentLifecycleSettings,
    labels:    HashMap<String, String>,
    volumes:   Vec<EnvironmentVolumeSettings>,
    env:       HashMap<String, InterpString>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiEnvironmentImageSettings {
    docker:     Option<String>,
    dockerfile: Option<ApiDockerfileSource>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ApiDockerfileSource {
    Inline {
        value: String,
    },
    // Recognized so the handler can return a 422 with bespoke guidance.
    // The `path` payload is parsed and discarded — never read from disk.
    Path {
        #[serde(rename = "path")]
        _path: IgnoredAny,
    },
}

impl CreateEnvironmentRequest {
    fn into_draft(self) -> Result<EnvironmentDraft, ApiError> {
        Ok(EnvironmentDraft {
            id:       self.id,
            settings: EnvironmentSettings {
                provider:  self.provider,
                image:     self.image.into_settings()?,
                resources: self.resources,
                network:   self.network,
                lifecycle: self.lifecycle,
                labels:    self.labels,
                volumes:   self.volumes,
                env:       self.env,
            },
        })
    }
}

impl ReplaceEnvironmentRequest {
    fn into_settings(self) -> Result<EnvironmentSettings, ApiError> {
        Ok(EnvironmentSettings {
            provider:  self.provider,
            image:     self.image.into_settings()?,
            resources: self.resources,
            network:   self.network,
            lifecycle: self.lifecycle,
            labels:    self.labels,
            volumes:   self.volumes,
            env:       self.env,
        })
    }
}

impl ApiEnvironmentImageSettings {
    fn into_settings(self) -> Result<EnvironmentImageSettings, ApiError> {
        Ok(EnvironmentImageSettings {
            docker:     self.docker,
            dockerfile: self
                .dockerfile
                .map(ApiDockerfileSource::into_settings)
                .transpose()?,
        })
    }
}

impl ApiDockerfileSource {
    fn into_settings(self) -> Result<DockerfileSource, ApiError> {
        match self {
            Self::Inline { value } => Ok(DockerfileSource::Inline(value)),
            Self::Path { .. } => Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Dockerfile path sources are not supported by the environments REST API; use inline Dockerfile content",
            )),
        }
    }
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/environments/{id}",
            get(get_environment)
                .put(replace_environment)
                .delete(delete_environment),
        )
}

async fn list_environments(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let data = state.environment_store().list();
    let total = data.len();
    (
        StatusCode::OK,
        Json(EnvironmentListResponse {
            data,
            meta: EnvironmentListMeta { total },
        }),
    )
        .into_response()
}

async fn create_environment(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateEnvironmentRequest>,
) -> Result<Response, ApiError> {
    let environment = state
        .environment_store()
        .create(request.into_draft()?)
        .await?;
    state.refresh_manifest_run_settings_from_environment_catalog();
    Ok((StatusCode::CREATED, Json(environment)).into_response())
}

async fn get_environment(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    match state.environment_store().get(&id) {
        Some(environment) => Ok(environment_with_etag_response(StatusCode::OK, environment)),
        None => Err(ApiError::not_found(format!("environment not found: {id}"))),
    }
}

async fn replace_environment(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ReplaceEnvironmentRequest>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "environment", &id)?;
    let environment = state
        .environment_store()
        .replace(&id, &expected, request.into_settings()?)
        .await?;
    state.refresh_manifest_run_settings_from_environment_catalog();
    Ok(environment_with_etag_response(StatusCode::OK, environment))
}

async fn delete_environment(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_path_id(id)?;
    let expected = parse_required_if_match(&headers, "environment", &id)?;
    state.environment_store().delete(&id, &expected).await?;
    state.refresh_manifest_run_settings_from_environment_catalog();
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn parse_path_id(id: String) -> Result<EnvironmentId, ApiError> {
    EnvironmentId::new(id)
        .map_err(|err| ApiError::bad_request(format!("invalid environment id: {err}")))
}

fn environment_with_etag_response(status: StatusCode, environment: Environment) -> Response {
    let revision = environment.revision.clone();
    json_with_etag_response(status, "environment", &revision, environment)
}

impl From<EnvironmentStoreError> for ApiError {
    fn from(err: EnvironmentStoreError) -> Self {
        match err {
            EnvironmentStoreError::NotFound { id } => {
                Self::not_found(format!("environment not found: {id}"))
            }
            EnvironmentStoreError::AlreadyExists { id } => Self::new(
                StatusCode::CONFLICT,
                format!("environment already exists: {id}"),
            ),
            EnvironmentStoreError::StaleRevision { id, .. } => Self::new(
                StatusCode::CONFLICT,
                format!("environment revision is stale: {id}"),
            ),
            EnvironmentStoreError::Protected { id } => Self::new(
                StatusCode::CONFLICT,
                format!("environment is protected and cannot be deleted: {id}"),
            ),
            EnvironmentStoreError::Validation { source } => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, source.to_string())
            }
            EnvironmentStoreError::InvalidFilename { .. }
            | EnvironmentStoreError::Parse { .. }
            | EnvironmentStoreError::InvalidUtf8 { .. }
            | EnvironmentStoreError::Serialize { .. }
            | EnvironmentStoreError::Io { .. } => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "environment store operation failed",
            ),
        }
    }
}
