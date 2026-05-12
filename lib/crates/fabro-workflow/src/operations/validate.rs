use std::path::PathBuf;

use fabro_types::WorkflowSettings;

use super::create::{RenderMode, preprocess_and_validate};
use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};
use crate::error::Error;
use crate::pipeline::Validated;
use crate::transforms::Transform;

pub struct ValidateInput {
    pub workflow:          WorkflowInput,
    pub settings:          WorkflowSettings,
    pub cwd:               PathBuf,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}

/// Parse, transform, and validate a DOT source string.
///
/// Returns `Validated` even when validation produced errors. Call
/// `validated.raise_on_errors()` if the caller wants to fail fast.
///
/// Uses [`RenderMode::Structural`]: undefined template inputs surface as
/// warning diagnostics rather than hard failures, so `fabro validate` can
/// type-check a bare `.fabro` graph before any inputs have been bound.
pub fn validate(input: ValidateInput) -> Result<Validated, Error> {
    let resolved = resolve_workflow(ResolveWorkflowInput {
        workflow: input.workflow,
        settings: input.settings,
        cwd:      input.cwd,
    })
    .map_err(|err| Error::Parse(err.to_string()))?;

    preprocess_and_validate(
        &resolved.raw_source,
        resolved.current_dir,
        resolved.file_resolver,
        input.custom_transforms,
        Some(&resolved.settings),
        resolved.goal_override.as_deref(),
        RenderMode::Structural,
    )
}
