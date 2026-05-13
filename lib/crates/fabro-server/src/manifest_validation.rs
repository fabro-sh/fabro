use std::sync::Arc;

use anyhow::Result;
use fabro_api::types;
use fabro_config::RunLayer;
use fabro_model::Catalog;
pub use fabro_workflow::operations::RenderMode;

use crate::run_manifest;

pub fn validate_manifest(
    manifest_run_defaults: &RunLayer,
    manifest: &types::RunManifest,
    mode: RenderMode,
    catalog: Arc<Catalog>,
) -> Result<types::ValidateResponse> {
    let prepared = run_manifest::prepare_manifest(manifest_run_defaults, manifest)?;
    let validated = run_manifest::validate_prepared_manifest(&prepared, mode, catalog)
        .map_err(anyhow::Error::new)?;
    Ok(run_manifest::validate_response(&prepared, &validated))
}
