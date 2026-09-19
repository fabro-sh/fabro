use std::collections::HashMap;
use std::path::PathBuf;

use fabro_types::WorkflowSettings;

use super::create::{preprocess_and_validate, template_context};
use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};
use crate::error::Error;
use crate::operations::RenderMode;
use crate::pipeline::{TransformOptions, Validated};
use crate::transforms::Transform;

pub struct ValidateInput {
    pub workflow:          WorkflowInput,
    pub settings:          WorkflowSettings,
    /// Run-scoped variables (`{{ vars.* }}`) available to prompts and goals.
    /// Empty for offline/CLI validation.
    pub vars:              HashMap<String, String>,
    pub cwd:               PathBuf,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}

/// Parse and transform a DOT source string: the structural validation Fabro
/// does itself. Its diagnostics are the transforms' (an unbound template
/// variable, a missing file); the workflow's rules and its models are
/// Petri's to judge at admission.
///
/// Returns `Validated` even when validation produced errors. Call
/// `validated.raise_on_errors()` if the caller wants to fail fast.
pub fn validate(input: ValidateInput) -> Result<Validated, Error> {
    let ValidateInput {
        workflow,
        settings,
        vars,
        cwd,
        custom_transforms,
    } = input;
    let resolved = resolve_workflow(ResolveWorkflowInput {
        workflow,
        settings,
        cwd,
    })
    .map_err(|err| Error::Parse(err.to_string()))?;

    preprocess_and_validate(
        &resolved.raw_source,
        resolved.goal_override.as_deref(),
        &TransformOptions {
            current_dir: resolved.current_dir,
            file_resolver: resolved.file_resolver,
            template_context: template_context(Some(&resolved.settings), vars),
            source_name: resolved
                .dot_path
                .as_ref()
                .map(|path| path.display().to_string()),
            render_mode: RenderMode::Structural,
            custom_transforms,
        },
    )
}
