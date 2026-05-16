use fabro_graphviz::graph::{self, Node};
use fabro_model::{AgentProfileKind, Catalog, ProviderId};
use fabro_types::LlmBackend;

use super::cli::is_cli_only_model;
use crate::error::Error;

pub(crate) fn select_run_backend(node: &Node) -> Result<LlmBackend, Error> {
    match node.llm_backend() {
        None => {
            if node.model().is_some_and(is_cli_only_model) {
                Ok(LlmBackend::Cli)
            } else {
                Ok(LlmBackend::Api)
            }
        }
        Some(Ok(backend)) => Ok(backend),
        Some(Err(_)) => Err(unsupported_backend_error(
            node.backend().unwrap_or_default(),
        )),
    }
}

pub(crate) fn select_one_shot_backend(node: &Node) -> Result<LlmBackend, Error> {
    match node.llm_backend() {
        Some(Ok(LlmBackend::Acp)) => Ok(LlmBackend::Acp),
        Some(Ok(LlmBackend::Api | LlmBackend::Cli)) | None => Ok(LlmBackend::Api),
        Some(Err(_)) => Err(unsupported_backend_error(
            node.backend().unwrap_or_default(),
        )),
    }
}

pub(crate) fn node_needs_api_backend(node: &Node) -> bool {
    if !graph::is_llm_handler_type(node.handler_type()) {
        return false;
    }

    match node.handler_type() {
        Some("prompt") => !matches!(select_one_shot_backend(node), Ok(LlmBackend::Acp)),
        _ => matches!(select_run_backend(node), Ok(LlmBackend::Api)),
    }
}

#[derive(Clone)]
pub(crate) struct ProviderContext {
    pub(crate) provider_id:  ProviderId,
    pub(crate) profile_kind: AgentProfileKind,
}

pub(crate) fn resolve_provider_context(
    catalog: &Catalog,
    default_provider_id: &ProviderId,
    model: &str,
    provider_attr: Option<&str>,
) -> Result<ProviderContext, Error> {
    let provider_id = if let Some(provider) = provider_attr {
        let requested = ProviderId::from(provider);
        catalog
            .provider(&requested)
            .ok_or_else(|| {
                Error::Precondition(format!("Provider \"{provider}\" is not configured"))
            })?
            .id
            .clone()
    } else if let Some(model) = catalog.get(model) {
        model.provider.clone()
    } else {
        default_provider_id.clone()
    };

    let provider = catalog.provider(&provider_id).ok_or_else(|| {
        Error::Precondition(format!("Provider \"{provider_id}\" is not configured"))
    })?;
    let profile_kind = catalog
        .effective_agent_profile(&provider.id, Some(model))
        .expect("validated provider should resolve an agent profile");
    Ok(ProviderContext {
        provider_id: provider.id.clone(),
        profile_kind,
    })
}

pub(crate) fn resolve_node_provider_context(
    catalog: &Catalog,
    default_provider_id: &ProviderId,
    default_model: &str,
    node: &Node,
) -> Result<ProviderContext, Error> {
    let model = node.model().unwrap_or(default_model);
    resolve_provider_context(catalog, default_provider_id, model, node.provider())
}

fn unsupported_backend_error(raw: &str) -> Error {
    Error::Validation(format!(
        "unsupported LLM backend \"{raw}\"; expected one of: {}",
        LlmBackend::expected_values()
    ))
}
