use std::sync::Arc;

use super::super::{
    ApiError, AppState, CreateSecretRequest, DeleteSecretRequest, IntoResponse, Json, RequiredUser,
    Response, Router, SecretType, State, StatusCode, VaultError, get, parse_credential_secret,
    spawn_blocking,
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
    let data = state.vault.read().await.list();
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
    if secret_type == SecretType::Credential {
        if let Err(err) = parse_credential_secret(&name, &value) {
            return ApiError::bad_request(err).into_response();
        }
    }
    let state_for_write = Arc::clone(&state);
    let result = spawn_blocking(move || {
        let mut vault = state_for_write.vault.blocking_write();
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
        let mut vault = state_for_write.vault.blocking_write();
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
