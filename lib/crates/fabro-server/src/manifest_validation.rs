use std::sync::Arc;

use anyhow::Result;
use fabro_api::types;
use fabro_config::{EnvironmentLayer, MergeMap, RunLayer};
use fabro_model::Catalog;
use fabro_workflow::pipeline::TEMPLATE_UNDEFINED_VARIABLE_RULE;

use crate::run_manifest;

pub fn validate_manifest(
    manifest_run_defaults: &RunLayer,
    manifest: &types::RunManifest,
    catalog: Arc<Catalog>,
) -> Result<types::ValidateResponse> {
    validate_manifest_with_environment_defaults(
        manifest_run_defaults,
        &fabro_environment::seeded_catalog_layer(),
        manifest,
        catalog,
    )
}

pub fn validate_manifest_with_environment_defaults(
    manifest_run_defaults: &RunLayer,
    manifest_environment_defaults: &MergeMap<EnvironmentLayer>,
    manifest: &types::RunManifest,
    catalog: Arc<Catalog>,
) -> Result<types::ValidateResponse> {
    let prepared = run_manifest::prepare_manifest_with_environment_defaults(
        manifest_run_defaults,
        manifest_environment_defaults,
        manifest,
    )?;
    let validated =
        run_manifest::validate_prepared_manifest(&prepared, catalog).map_err(anyhow::Error::new)?;
    Ok(run_manifest::validate_response(&prepared, &validated))
}

pub fn promote_template_undefined_variables_to_errors(response: &mut types::ValidateResponse) {
    let mut promoted = false;
    for diagnostic in &mut response.workflow.diagnostics {
        if diagnostic.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE {
            diagnostic.severity = types::WorkflowDiagnosticSeverity::Error;
            promoted = true;
        }
    }
    if promoted {
        response.ok = false;
    }
}
