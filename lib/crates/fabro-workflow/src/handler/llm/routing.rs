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
        Some("prompt" | "one_shot") => {
            !matches!(select_one_shot_backend(node), Ok(LlmBackend::Acp))
        }
        _ => matches!(select_run_backend(node), Ok(LlmBackend::Api)),
    }
}

#[derive(Clone)]
pub(super) struct ProviderContext {
    pub(super) provider_id:  ProviderId,
    pub(super) profile_kind: AgentProfileKind,
}

pub(super) fn default_profile_kind(
    catalog: &Catalog,
    provider_id: &ProviderId,
) -> AgentProfileKind {
    catalog
        .provider(provider_id)
        .unwrap_or_else(|| panic!("Provider \"{provider_id}\" is not configured"))
        .adapter
        .metadata()
        .default_profile
}

pub(super) fn resolve_provider_context(
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
    Ok(ProviderContext {
        provider_id:  provider.id.clone(),
        profile_kind: provider.adapter.metadata().default_profile,
    })
}

fn unsupported_backend_error(raw: &str) -> Error {
    Error::Validation(format!(
        "unsupported LLM backend \"{raw}\"; expected one of: {}",
        LlmBackend::expected_values()
    ))
}
