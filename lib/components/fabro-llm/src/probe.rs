//! Model and provider probes for the server's test endpoints.

use std::sync::Arc;
use std::time::Duration;

use fabro_auth::ApiKeyCredentialSource;
use fabro_types::ModelTestMode;
use lithos_llm::catalog::{Catalog, ProviderId};
use lithos_llm::client::{Client, ProbeOptions, ProbeOutcome};
use lithos_llm::types::ReasoningEffort;
use strum::IntoStaticStr;

use crate::client::{ClientOptions, LlmSetupError, build_client};

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum ModelTestStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTestOutcome {
    pub status:        ModelTestStatus,
    pub error_message: Option<String>,
}

impl ModelTestOutcome {
    #[must_use]
    pub fn ok() -> Self {
        Self {
            status:        ModelTestStatus::Ok,
            error_message: None,
        }
    }

    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status:        ModelTestStatus::Error,
            error_message: Some(message.into()),
        }
    }
}

/// Probes `selector` (a `provider/model` route or bare selector) in `mode`.
///
/// `Basic` asks for one word; `Deep` runs a two-step tool exchange and checks
/// the total. The probe resolves, authenticates, and retries exactly as a
/// real request would.
pub async fn run_model_test(
    client: &Client,
    selector: &str,
    mode: ModelTestMode,
    reasoning_effort: Option<ReasoningEffort>,
    timeout: Option<Duration>,
) -> ModelTestOutcome {
    let mut options = ProbeOptions::new()
        .tools(mode == ModelTestMode::Deep)
        .timeout(timeout.unwrap_or_else(|| Duration::from_secs(mode.timeout_secs())));
    if let Some(effort) = reasoning_effort {
        options = options.reasoning_effort(effort);
    }
    let report = client.probe(selector, options).await;
    match report.outcome {
        ProbeOutcome::Passed => ModelTestOutcome::ok(),
        ProbeOutcome::Failed(data) => ModelTestOutcome::error(data.message),
        ProbeOutcome::Incorrect { detail } => ModelTestOutcome::error(detail),
        _ => ModelTestOutcome::error("probe ended in an unknown state"),
    }
}

/// A basic probe of `selector` bounded by `timeout`.
pub async fn run_basic_probe(
    client: &Client,
    selector: &str,
    timeout: Duration,
) -> ModelTestOutcome {
    run_model_test(client, selector, ModelTestMode::Basic, None, Some(timeout)).await
}

/// Why an API key could not be probed.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyProbeError {
    #[error("provider '{0}' is not configured in the model catalog")]
    UnknownProvider(String),
    #[error("provider '{0}' does not define an API-key credential path")]
    NoApiKeyPath(ProviderId),
    #[error("provider '{0}' does not define a probe model")]
    NoProbeModel(ProviderId),
    #[error(transparent)]
    Setup(#[from] LlmSetupError),
}

/// Probes `provider` with an operator-supplied `api_key`, the check behind
/// `fabro provider login`, the install flow, and the credential test API.
///
/// The key is shaped into the provider's auth scheme and used for the
/// provider's probe model. The result says whether the key works; the caller
/// decides whether to store it.
pub async fn probe_provider_with_api_key(
    catalog: Catalog,
    provider: &ProviderId,
    api_key: String,
    timeout: Duration,
) -> Result<ModelTestOutcome, ApiKeyProbeError> {
    let catalog_provider = catalog
        .enabled_provider(provider.as_str())
        .ok_or_else(|| ApiKeyProbeError::UnknownProvider(provider.to_string()))?;
    let provider_id = catalog_provider.id().clone();
    if !fabro_auth::accepts_api_key(catalog_provider) {
        return Err(ApiKeyProbeError::NoApiKeyPath(provider_id));
    }
    let model = catalog_provider
        .probe_offering()
        .ok_or_else(|| ApiKeyProbeError::NoProbeModel(provider_id.clone()))?;
    let selector = model.handle().to_string();
    let source = Arc::new(ApiKeyCredentialSource::new(provider_id.clone(), api_key));
    let built = build_client(catalog, source, ClientOptions::standard()).await?;
    if let Some((_, issue)) = built
        .auth_issues
        .iter()
        .find(|(candidate, _)| candidate == &provider_id)
    {
        return Ok(ModelTestOutcome::error(issue.to_string()));
    }
    Ok(run_basic_probe(&built.client, &selector, timeout).await)
}
