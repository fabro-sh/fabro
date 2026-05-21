use std::path::Path;
use std::sync::Arc;

use fabro_api::types;
use fabro_config::load_llm_catalog_settings;
use fabro_model::Catalog;
use fabro_server::run_tool_manifest;
use fabro_tool::{RunManifestBuilder, ToolError, ToolResult, ValidatedCreateRunSpec};

#[derive(Default)]
pub(crate) struct McpRunManifestBuilder;

impl RunManifestBuilder for McpRunManifestBuilder {
    fn build_run_manifest(
        &self,
        spec: &ValidatedCreateRunSpec,
        cwd: &Path,
        user_settings_path: &Path,
    ) -> ToolResult<types::RunManifest> {
        build_mcp_run_manifest(spec, cwd, user_settings_path)
    }
}

fn build_mcp_run_manifest(
    spec: &ValidatedCreateRunSpec,
    cwd: &Path,
    user_settings_path: &Path,
) -> ToolResult<types::RunManifest> {
    let llm_catalog_settings = load_llm_catalog_settings(Some(user_settings_path))
        .map_err(|err| ToolError::message(err.to_string()))?;
    let catalog = Arc::new(
        Catalog::from_builtin_with_overrides(&llm_catalog_settings)
            .map_err(|err| ToolError::message(err.to_string()))?,
    );
    run_tool_manifest::build_run_tool_manifest(spec, cwd, user_settings_path, catalog)
}
