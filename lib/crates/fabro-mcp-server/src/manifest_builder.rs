use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_api::types;
use fabro_config::{CliLayer, RunLayer, load_llm_catalog_settings};
use fabro_manifest::{self, ManifestBuildInput, RunOverrideInput};
use fabro_model::Catalog;
use fabro_server::manifest_validation;
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
    let built = fabro_manifest::build_run_manifest(ManifestBuildInput {
        workflow:           PathBuf::from(&spec.workflow),
        cwd:                cwd.to_path_buf(),
        run_overrides:      mcp_run_overrides(spec),
        cli_overrides:      Some(CliLayer::default()),
        input_overrides:    spec.inputs.clone(),
        args:               mcp_manifest_args(spec),
        run_id:             spec.run_id,
        user_settings_path: Some(user_settings_path.to_path_buf()),
    })
    .map_err(|err| ToolError::from_anyhow(&err))?;
    let llm_catalog_settings = load_llm_catalog_settings(Some(user_settings_path))
        .map_err(|err| ToolError::message(err.to_string()))?;
    let catalog = Arc::new(
        Catalog::from_builtin_with_overrides(&llm_catalog_settings)
            .map_err(|err| ToolError::message(err.to_string()))?,
    );
    let mut validation =
        manifest_validation::validate_manifest(&RunLayer::default(), &built.manifest, catalog)
            .map_err(|err| ToolError::from_anyhow(&err))?;
    manifest_validation::promote_template_undefined_variables_to_errors(&mut validation);
    if !validation.ok {
        return Err(ToolError::message("workflow manifest validation failed"));
    }
    Ok(built.manifest)
}

fn mcp_manifest_args(spec: &ValidatedCreateRunSpec) -> Option<types::ManifestArgs> {
    let mut input = spec
        .inputs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    input.sort();
    let mut label = spec
        .labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    label.sort();
    let payload = types::ManifestArgs {
        auto_approve: spec.auto_approve.filter(|value| *value),
        docker_image: None,
        dry_run: spec.dry_run.filter(|value| *value),
        input,
        label,
        model: spec.model.clone(),
        preserve_sandbox: spec.preserve_sandbox.filter(|value| *value),
        provider: spec.provider.clone(),
        sandbox: spec.sandbox.clone(),
        verbose: None,
    };
    (!fabro_manifest::manifest_args_is_empty(&payload)).then_some(payload)
}

fn mcp_run_overrides(spec: &ValidatedCreateRunSpec) -> Option<RunLayer> {
    fabro_manifest::build_sparse_run_overrides(RunOverrideInput {
        goal:             spec.goal.as_deref(),
        model:            spec.model.as_deref(),
        provider:         spec.provider.as_deref(),
        sandbox:          spec.sandbox.as_deref(),
        docker_image:     None,
        preserve_sandbox: spec.preserve_sandbox,
        dry_run:          spec.dry_run,
        auto_approve:     spec.auto_approve,
        labels:           spec.labels.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_tool::{CreateRunSpec, ValidatedCreateRunSpec};
    use serde_json::json;

    use super::*;

    #[test]
    fn mcp_manifest_args_preserve_input_provenance() {
        let spec = ValidatedCreateRunSpec::try_from(CreateRunSpec {
            workflow:         "simple".to_string(),
            run_id:           None,
            parent_id:        None,
            cwd:              None,
            goal:             None,
            inputs:           HashMap::from([
                ("count".to_string(), json!(3).into()),
                ("decision".to_string(), json!("approve").into()),
            ]),
            labels:           HashMap::new(),
            model:            None,
            provider:         None,
            sandbox:          None,
            dry_run:          None,
            auto_approve:     None,
            preserve_sandbox: None,
            start:            None,
        })
        .expect("create spec should validate");
        let args = mcp_manifest_args(&spec).expect("input args should be present");

        assert_eq!(args.input, vec![r"count=3", r#"decision="approve""#]);
    }
}
