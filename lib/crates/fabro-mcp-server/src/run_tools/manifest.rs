use std::path::{Path, PathBuf};

use fabro_api::types;
use fabro_config::{CliLayer, RunLayer, load_llm_catalog_settings};
use fabro_manifest::{self, ManifestBuildInput, RunOverrideInput};
use fabro_model::Catalog;
use fabro_server::manifest_validation::{self, RenderMode};
use serde_json::Value;

use super::common::{ToolError, ToolResult};
use super::create::ValidatedCreateRunSpec;

pub(super) fn build_mcp_run_manifest(
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
    let catalog = std::sync::Arc::new(
        Catalog::from_builtin_with_overrides(&llm_catalog_settings)
            .map_err(|err| ToolError::message(err.to_string()))?,
    );
    let validation = manifest_validation::validate_manifest(
        &RunLayer::default(),
        &built.manifest,
        RenderMode::Strict,
        catalog,
    )
    .map_err(|err| ToolError::from_anyhow(&err))?;
    if !validation.ok {
        return Err(ToolError::message("workflow manifest validation failed"));
    }
    Ok(built.manifest)
}

pub(super) fn json_to_toml_value(key: &str, value: &Value) -> ToolResult<toml::Value> {
    match value {
        Value::Null => Err(ToolError::message(format!(
            "input `{key}` cannot be null; use a string, boolean, or number"
        ))),
        Value::Bool(value) => Ok(toml::Value::Boolean(*value)),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Ok(toml::Value::Integer(integer))
            } else if let Some(float) = value.as_f64() {
                Ok(toml::Value::Float(float))
            } else {
                Err(ToolError::message(format!(
                    "input `{key}` contains a number outside TOML's supported range"
                )))
            }
        }
        Value::String(value) => Ok(toml::Value::String(value.clone())),
        Value::Array(_) => Err(ToolError::message(format!(
            "input `{key}` does not support array values; use a string, boolean, or number",
        ))),
        Value::Object(_) => Err(ToolError::message(format!(
            "input `{key}` does not support object values; use a string, boolean, or number",
        ))),
    }
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

    use serde_json::{Value, json};

    use super::super::create::CreateRunSpec;
    use super::*;

    #[test]
    fn json_inputs_convert_scalar_values_to_toml_values() {
        let cases = [
            (json!("hello"), toml::Value::String("hello".to_string())),
            (json!(true), toml::Value::Boolean(true)),
            (json!(42), toml::Value::Integer(42)),
            (json!(0.5), toml::Value::Float(0.5)),
        ];

        for (json, expected) in cases {
            assert_eq!(json_to_toml_value("input", &json).unwrap(), expected);
        }
    }

    #[test]
    fn json_input_arrays_and_objects_are_rejected() {
        let array_err = json_to_toml_value("matrix", &json!(["a", 1])).unwrap_err();
        assert_eq!(
            array_err.as_str(),
            "input `matrix` does not support array values; use a string, boolean, or number",
        );

        let object_err = json_to_toml_value("settings", &json!({ "enabled": true })).unwrap_err();
        assert_eq!(
            object_err.as_str(),
            "input `settings` does not support object values; use a string, boolean, or number",
        );
    }

    #[test]
    fn json_input_null_is_rejected_with_key_name() {
        let err = json_to_toml_value("goal", &Value::Null).unwrap_err();

        assert_eq!(
            err.as_str(),
            "input `goal` cannot be null; use a string, boolean, or number",
        );
    }

    #[test]
    fn mcp_manifest_args_preserve_input_provenance() {
        let spec = ValidatedCreateRunSpec::try_from(CreateRunSpec {
            workflow:         "simple".to_string(),
            run_id:           None,
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
