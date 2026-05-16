use std::path::PathBuf;
use std::sync::Arc;

use fabro_model::Catalog;
use fabro_types::WorkflowSettings;

use super::create::preprocess_and_validate;
use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};
use crate::error::Error;
use crate::operations::RenderMode;
use crate::pipeline::Validated;
use crate::transforms::Transform;

pub struct ValidateInput {
    pub workflow:          WorkflowInput,
    pub settings:          WorkflowSettings,
    pub cwd:               PathBuf,
    pub custom_transforms: Vec<Box<dyn Transform>>,
    pub catalog:           Arc<Catalog>,
}

/// Parse, transform, and validate a DOT source string.
///
/// Returns `Validated` even when validation produced errors. Call
/// `validated.raise_on_errors()` if the caller wants to fail fast.
pub fn validate(input: ValidateInput) -> Result<Validated, Error> {
    let resolved = resolve_workflow(ResolveWorkflowInput {
        workflow: input.workflow,
        settings: input.settings,
        cwd:      input.cwd,
    })
    .map_err(|err| Error::Parse(err.to_string()))?;

    preprocess_and_validate(
        &resolved.raw_source,
        resolved
            .dot_path
            .as_ref()
            .map(|path| path.display().to_string()),
        resolved.current_dir,
        resolved.file_resolver,
        input.custom_transforms,
        Some(&resolved.settings),
        resolved.goal_override.as_deref(),
        RenderMode::Structural,
        &input.catalog,
    )
}
