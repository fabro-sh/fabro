use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use fabro_llm::api;
use fabro_llm::lithos_catalog::Catalog;
use fabro_llm::probe::{self, ApiKeyProbeError, ModelTestStatus};
use fabro_redact::redact_string;
use lithos_llm::client::{ProbeOptions, ProbeOutcome};
use lithos_llm::types::ReasoningEffort;

use super::super::{
    ApiError, AppState, FromStr, IntoResponse, Json, MAX_PAGE_OFFSET, ModelTestMode, Path,
    ProviderCredentialTestRequest, ProviderCredentialTestResponse, ProviderId, ProviderList, Query,
    RequiredUser, Response, Router, State, StatusCode, default_page_limit, error, get, post,
};
use crate::diagnostics;

const CREDENTIAL_TEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/models", get(list_models))
        .route("/models/{id}/test", post(test_model))
        .route("/providers", get(list_providers))
        .route(
            "/providers/{provider}/credentials/test",
            post(test_provider_credentials),
        )
        .route("/providers/test", post(test_providers))
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
    mode:             Option<String>,
    #[serde(default)]
    provider:         Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

async fn list_models(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ModelListParams>,
) -> Response {
    let catalog = state.catalog();
    // An unknown provider filter matches nothing rather than erroring.
    let provider_id = params.provider.as_deref().map(|selector| {
        catalog.enabled_provider(selector).map_or_else(
            || ProviderId::new(selector),
            |provider| provider.id().clone(),
        )
    });

    let query = params.query.as_ref().map(|value| value.to_lowercase());
    let limit = params.limit.clamp(1, 100) as usize;
    let offset = params.offset.min(MAX_PAGE_OFFSET) as usize;
    let configured: HashSet<ProviderId> =
        state.ready_llm_provider_ids().await.into_iter().collect();

    let mut data = api::models(&catalog, &configured)
        .into_iter()
        .filter(|model| {
            provider_id
                .as_ref()
                .is_none_or(|provider| &model.provider == provider)
        })
        .filter(|model| match &query {
            Some(query) => {
                model.id.as_str().to_lowercase().contains(query)
                    || model.display_name.to_lowercase().contains(query)
                    || model
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(query))
            }
            None => true,
        })
        .skip(offset)
        .take(limit + 1)
        .collect::<Vec<_>>();

    let has_more = data.len() > limit;
    data.truncate(limit);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": data,
            "meta": { "has_more": has_more }
        })),
    )
        .into_response()
}

async fn list_providers(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let catalog = state.catalog();
    let configured: HashSet<ProviderId> = state
        .configured_llm_provider_ids()
        .await
        .into_iter()
        .collect();
    let data = api::providers(&catalog, &configured);

    (StatusCode::OK, Json(ProviderList { data })).into_response()
}

async fn test_provider_credentials(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    Json(body): Json<ProviderCredentialTestRequest>,
) -> Response {
    if body.api_key.trim().is_empty() {
        return ApiError::bad_request("api_key is required").into_response();
    }

    let requested_provider = ProviderId::new(provider);
    let catalog = state.catalog();
    let outcome = match probe::probe_provider_with_api_key(
        Catalog::clone(&catalog),
        &requested_provider,
        body.api_key,
        CREDENTIAL_TEST_TIMEOUT,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(ApiKeyProbeError::UnknownProvider(_)) => {
            return ApiError::not_found(format!("Provider not found: {requested_provider}"))
                .into_response();
        }
        Err(err @ (ApiKeyProbeError::NoApiKeyPath(_) | ApiKeyProbeError::NoProbeModel(_))) => {
            return ApiError::bad_request(err.to_string()).into_response();
        }
        Err(ApiKeyProbeError::Setup(err)) => {
            error!(provider = %requested_provider, error = ?err, "Failed to create LLM client for provider credential validation");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {err}"),
            )
            .into_response();
        }
    };
    match outcome.status {
        ModelTestStatus::Ok => (
            StatusCode::OK,
            Json(ProviderCredentialTestResponse { ok: true }),
        )
            .into_response(),
        ModelTestStatus::Error => {
            let message = outcome
                .error_message
                .unwrap_or_else(|| "provider credential validation failed".to_string());
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, redact_string(&message)).into_response()
        }
    }
}

async fn test_providers(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    match diagnostics::test_llm_providers(&state).await {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(err) => {
            error!(error = ?err, "Failed to resolve LLM providers for provider test");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to resolve LLM providers",
            )
            .into_response()
        }
    }
}

fn parse_query_enum<T: FromStr>(value: Option<&str>, label: &str) -> Result<Option<T>, ApiError> {
    value
        .map(|value| {
            T::from_str(value).map_err(|_| {
                ApiError::new(StatusCode::BAD_REQUEST, format!("invalid {label}: {value}"))
            })
        })
        .transpose()
}

async fn test_model(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ModelTestParams>,
) -> Response {
    let mode = match parse_query_enum(params.mode.as_deref(), "model test mode") {
        Ok(mode) => mode.unwrap_or(ModelTestMode::Basic),
        Err(error) => return error.into_response(),
    };
    let reasoning_effort = match params.reasoning_effort.as_deref() {
        Some(value) => match value.parse::<ReasoningEffort>() {
            Ok(effort) => Some(effort),
            Err(_) => {
                return ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid reasoning effort: {value}"),
                )
                .into_response();
            }
        },
        None => None,
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
    let selector = params
        .provider
        .map_or_else(|| id.clone(), |provider| format!("{provider}/{id}"));
    let mut options = ProbeOptions::new().tools(mode == ModelTestMode::Deep);
    if let Some(effort) = reasoning_effort {
        options = options.reasoning_effort(effort);
    }
    let mut report = llm_result.client.probe(selector, options).await;
    // Model selection and probe findings belong to lithos. Fabro only owns
    // disclosure of provider diagnostics across its HTTP boundary.
    match &mut report.outcome {
        ProbeOutcome::Failed(error) => {
            error.message = redact_string(&error.message);
            error.provider_code = error.provider_code.as_deref().map(redact_string);
            error.raw_data = None;
            error.source_message = None;
        }
        ProbeOutcome::Incorrect { detail } => *detail = redact_string(detail),
        _ => {}
    }
    Json(report).into_response()
}
