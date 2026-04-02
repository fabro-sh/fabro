use std::path::PathBuf;

use fabro_types::Settings;

use crate::error::FabroError;
use crate::pipeline::Validated;
use crate::transforms::Transform;

use super::create::preprocess_and_validate;
use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};

pub struct ValidateInput {
    pub workflow: WorkflowInput,
    pub settings: Settings,
    pub cwd: PathBuf,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}

/// Parse, transform, and validate a DOT source string.
///
/// Returns `Validated` even when validation produced errors. Call
/// `validated.raise_on_errors()` if the caller wants to fail fast.
pub fn validate(input: ValidateInput) -> Result<Validated, FabroError> {
    let resolved = resolve_workflow(ResolveWorkflowInput {
        workflow: input.workflow,
        settings: input.settings,
        cwd: input.cwd,
    })
    .map_err(|err| FabroError::Parse(err.to_string()))?;

    preprocess_and_validate(
        &resolved.raw_source,
        resolved.base_dir,
        input.custom_transforms,
        Some(&resolved.settings),
        resolved.goal_override.as_deref(),
    )
}
