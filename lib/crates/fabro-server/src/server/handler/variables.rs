use std::sync::Arc;

use super::super::{
    ApiError, AppState, CreateVariableRequest, IntoResponse, Json, Path, RequiredUser, Response,
    Router, State, StatusCode, UpdateVariableRequest, VariableError, VariableListResponse,
    VariableStore, get,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/variables", get(list_variables).post(create_variable))
        .route(
            "/variables/{name}",
            get(get_variable)
                .put(update_variable)
                .delete(delete_variable),
        )
}

async fn list_variables(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    match state.stores.variables.list().await {
        Ok(data) => (StatusCode::OK, Json(VariableListResponse { data })).into_response(),
        Err(err) => variable_error_response(err),
    }
}

async fn create_variable(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateVariableRequest>,
) -> Response {
    match state
        .stores
        .variables
        .set(&body.name, &body.value, body.description.as_deref())
        .await
    {
        Ok(variable) => (StatusCode::OK, Json(variable)).into_response(),
        Err(err) => variable_error_response(err),
    }
}

async fn get_variable(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    if let Err(VariableError::InvalidName(_)) = VariableStore::validate_name(&name) {
        return ApiError::bad_request("invalid variable name").into_response();
    }
    match state.stores.variables.get(&name).await {
        Ok(Some(variable)) => (StatusCode::OK, Json(variable)).into_response(),
        Ok(None) => ApiError::not_found(format!("variable not found: {name}")).into_response(),
        Err(err) => variable_error_response(err),
    }
}

async fn update_variable(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<UpdateVariableRequest>,
) -> Response {
    match state
        .stores
        .variables
        .update_existing(&name, &body.value, body.description.as_deref())
        .await
    {
        Ok(variable) => (StatusCode::OK, Json(variable)).into_response(),
        Err(err) => variable_error_response(err),
    }
}

async fn delete_variable(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.stores.variables.remove(&name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => variable_error_response(err),
    }
}

fn variable_error_response(err: VariableError) -> Response {
    match err {
        VariableError::InvalidName(_) => {
            ApiError::bad_request("invalid variable name").into_response()
        }
        VariableError::NotFound(name) => {
            ApiError::not_found(format!("variable not found: {name}")).into_response()
        }
        VariableError::Db(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        VariableError::LegacyRead { .. }
        | VariableError::LegacyParse { .. }
        | VariableError::LegacyInvalidName { .. }
        | VariableError::LegacyBackup { .. }
        | VariableError::Timestamp { .. }
        | VariableError::RowCountOverflow { .. } => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}
