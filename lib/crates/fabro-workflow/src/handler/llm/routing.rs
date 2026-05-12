use fabro_graphviz::graph::{self, Node};
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

fn unsupported_backend_error(raw: &str) -> Error {
    Error::Validation(format!(
        "unsupported LLM backend \"{raw}\"; expected one of: {}",
        LlmBackend::expected_values()
    ))
}
