use std::sync::Arc;

use fabro_auth::OAuthCredential;
use fabro_static::EnvVars;

use super::super::{
    ApiError, AppState, CreateSecretRequest, DeleteSecretRequest, IntoResponse, Json, RequiredUser,
    Response, Router, SecretType, State, StatusCode, VaultError, get, spawn_blocking,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/secrets",
        get(list_secrets)
            .post(create_secret)
            .delete(delete_secret_by_name),
    )
}

async fn list_secrets(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let data = state.stores.vault.read().await.list();
    (StatusCode::OK, Json(serde_json::json!({ "data": data }))).into_response()
}

async fn create_secret(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSecretRequest>,
) -> Response {
    let secret_type = body.type_;
    let name = body.name;
    let value = body.value;
    let description = body.description;
    if fabro_static::is_bootstrap_secret(&name) {
        return ApiError::bad_request(format!(
            "{name} is a bootstrap secret; configure it with process env or server.env"
        ))
        .into_response();
    }
    if secret_type == SecretType::Oauth {
        if let Err(err) = serde_json::from_str::<OAuthCredential>(&value) {
            return ApiError::bad_request(format!("invalid oauth credential JSON: {err}"))
                .into_response();
        }
    }
    if secret_type == SecretType::Token && name == EnvVars::DAYTONA_API_KEY {
        match state.check_daytona_api_key(value.clone()).await {
            Ok(check) if check.ok() => {}
            Ok(check) => {
                return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, check.missing_message())
                    .into_response();
            }
            Err(err) => {
                return ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("daytona credential validation failed: {err}"),
                )
                .into_response();
            }
        }
    }
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut vault = state_for_write.stores.vault.blocking_write();
        vault.set(&name, &value, secret_type, description.as_deref())
    })
    .await;

    match result {
        Ok(Ok(meta)) => (StatusCode::OK, Json(meta)).into_response(),
        Ok(Err(VaultError::InvalidName(_))) => {
            ApiError::bad_request("invalid secret name").into_response()
        }
        Ok(Err(VaultError::Io(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(VaultError::Serde(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(VaultError::NotFound(_))) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret unexpectedly missing",
        )
        .into_response(),
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("secret write task failed: {err}"),
        )
        .into_response(),
    }
}

async fn delete_secret_by_name(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeleteSecretRequest>,
) -> Response {
    let name = body.name;
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut vault = state_for_write.stores.vault.blocking_write();
        vault.remove(&name)
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(VaultError::InvalidName(_))) => {
            ApiError::bad_request("invalid secret name").into_response()
        }
        Ok(Err(VaultError::NotFound(name))) => {
            ApiError::new(StatusCode::NOT_FOUND, format!("secret not found: {name}"))
                .into_response()
        }
        Ok(Err(VaultError::Io(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(Err(VaultError::Serde(err))) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Err(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("secret delete task failed: {err}"),
        )
        .into_response(),
    }
}
