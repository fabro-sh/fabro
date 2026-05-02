use std::sync::Arc;

use super::super::{
    ApiError, AppState, FromStr, HashSet, IntoResponse, Json, MAX_PAGE_OFFSET, ModelTestMode, Path,
    Provider, Query, RequiredUser, Response, Router, State, StatusCode, auth_issue_message,
    default_page_limit, error, get, post, run_model_test,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/models", get(list_models))
        .route("/models/{id}/test", post(test_model))
}

#[derive(serde::Deserialize)]
struct ModelListParams {
    #[serde(rename = "page[limit]", default = "default_page_limit")]
    limit:    u32,
    #[serde(rename = "page[offset]", default)]
    offset:   u32,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    query:    Option<String>,
}

#[derive(serde::Deserialize)]
struct ModelTestParams {
    #[serde(default)]
    mode: Option<String>,
}

async fn list_models(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ModelListParams>,
) -> Response {
    let provider = match params.provider.as_deref() {
        Some(value) => match Provider::from_str(value) {
            Ok(provider) => Some(provider),
            Err(_) => {
                return ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unknown provider: {value}"),
                )
                .into_response();
            }
        },
        None => None,
    };

    let query = params.query.as_ref().map(|value| value.to_lowercase());
    let limit = params.limit.clamp(1, 100) as usize;
    let offset = params.offset.min(MAX_PAGE_OFFSET) as usize;
    let configured: HashSet<Provider> = state
        .llm_source
        .configured_providers()
        .await
        .into_iter()
        .collect();

    let mut models = fabro_model::Catalog::builtin()
        .list(provider)
        .into_iter()
        .filter(|model| match &query {
            Some(query) => {
                model.id.to_lowercase().contains(query)
                    || model.display_name.to_lowercase().contains(query)
                    || model
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(query))
            }
            None => true,
        })
        .cloned()
        .map(|mut model| {
            model.configured = configured.contains(&model.provider);
            model
        })
        .collect::<Vec<_>>();

    let has_more = models.len() > offset.saturating_add(limit);
    let data = models.drain(offset..models.len().min(offset.saturating_add(limit)));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data.collect::<Vec<_>>(),
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn test_model(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ModelTestParams>,
) -> Response {
    let mode = match params.mode.as_deref() {
        Some(value) => match ModelTestMode::from_str(value) {
            Ok(mode) => mode,
            Err(_) => {
                return ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid model test mode: {value}"),
                )
                .into_response();
            }
        },
        None => ModelTestMode::Basic,
    };
    let Some(info) = fabro_model::Catalog::builtin().get(&id) else {
        return ApiError::not_found(format!("Model not found: {id}")).into_response();
    };

    let llm_result = match state.resolve_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            error!(error = ?err, "Failed to resolve LLM client");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve LLM client: {err}"),
            )
            .into_response();
        }
    };
    if let Some((_, issue)) = llm_result
        .auth_issues
        .iter()
        .find(|(provider, _)| *provider == info.provider)
    {
        return ApiError::bad_request(auth_issue_message(info.provider, issue)).into_response();
    }
    let provider_name = <&'static str>::from(info.provider);
    if !llm_result.client.provider_names().contains(&provider_name) {
        return Json(serde_json::json!({
            "model_id": info.id,
            "status": "skip",
        }))
        .into_response();
    }
    let client = Arc::new(llm_result.client);

    let outcome = run_model_test(info, mode, client).await;
    Json(serde_json::json!({
        "model_id": info.id,
        "status": <&'static str>::from(outcome.status),
        "error_message": outcome.error_message,
    }))
    .into_response()
}
