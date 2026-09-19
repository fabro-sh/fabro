//! The create-time run compiler: the single pipeline that turns an acquired
//! workflow bundle into a complete, persistable run.
//!
//! The pipeline has four stages, each its own function with typed input and
//! output:
//!
//! 1. [`normalize_source`] — resolve the bundle entrypoint and retain the
//!    admitted workflow settings, whose dockerfile references are already
//!    inlined.
//! 2. [`layer_settings`] + [`apply_run_variables`] + graph compilation — layer
//!    settings from every configured source, substitute the run-scoped variable
//!    snapshot, then parse/transform/validate the graph through the
//!    fabro-workflow pipeline.
//! 3. [`compile_admitted`] — Petri compiled, linted and pinned models at its
//!    admission, so only the Fabro graph the read side displays is parsed here,
//!    and the admission is recorded on the run.
//! 4. [`assemble_run`] — purely assemble the complete persistence input; no
//!    field is mutated after assembly.
//!
//! The input is deliberately source-neutral: it speaks in terms of an
//! acquired [`WorkflowBundle`], not any wire request type, so non-HTTP
//! callers and alternative workflow sources can drive the same pipeline.
//! Callers own source acquisition, run-id resolution, variable snapshotting,
//! and (for HTTP callers) all wire mapping — including turning
//! [`RunCompilerError`] into HTTP responses.

use std::collections::HashMap;
use std::path::PathBuf;

use fabro_config::parse::{self, ParseError, SettingsSource};
use fabro_config::{
    EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer, MergeMap, RunLayer,
    SettingsLayer, WorkflowSettingsBuilder,
};
use fabro_types::settings::interp::{InterpString, ResolveError};
use fabro_types::settings::run::{McpServerSettings, RunGoal};
use fabro_types::{
    AutomationRef, GitContext, ManifestPath, PetriAdmission, RunId, RunProvenance, RunTarget,
    WorkflowSettings, WorkflowVersionId,
};
use fabro_util::workspace_glob::{WorkspaceGlob, WorkspaceGlobError};
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::operations::{
    self, CreateRunCompileInput, CreateRunPersistenceInput, CreateRunPersistenceMetadata,
    MaterializedRun, WorkflowInput,
};
use fabro_workflow::workflow_bundle::{BundledWorkflow, WorkflowBundle};
use tokio::task;

/// Transport-neutral inputs for compiling one submitted run.
///
/// Identity (`run_id`), lineage, title, git metadata, and provenance are
/// resolved by the caller. This boundary owns only source normalization,
/// settings resolution, workflow compilation, model pinning, and
/// persistence-input assembly.
#[derive(Debug)]
pub(crate) struct RawRunCompilerInput {
    pub(crate) workflow_bundle: WorkflowBundle,
    pub(crate) entrypoint: ManifestPath,
    pub(crate) cwd: PathBuf,
    pub(crate) server_run_defaults: RunLayer,
    pub(crate) server_environment_defaults: MergeMap<EnvironmentLayer>,
    pub(crate) server_mcp_catalog: HashMap<String, McpServerSettings>,
    pub(crate) workflow_layer: Option<SettingsLayer>,
    pub(crate) run_overrides: Option<RunLayer>,
    pub(crate) input_overrides: HashMap<String, toml::Value>,
    pub(crate) inline_goal_override: Option<String>,
    pub(crate) run_id: Option<RunId>,
    pub(crate) title: Option<String>,
    pub(crate) parent_id: Option<RunId>,
    pub(crate) git: Option<GitContext>,
    pub(crate) storage_root: PathBuf,
    pub(crate) workflow_slug: Option<String>,
    pub(crate) workflow_version_id: Option<WorkflowVersionId>,
    pub(crate) target: Option<RunTarget>,
    pub(crate) provenance: RunProvenance,
    pub(crate) web_url: Option<String>,
    pub(crate) automation: Option<AutomationRef>,
}

/// Stage-one output: the selected bundled workflow and admitted settings,
/// before layering server defaults and run overrides.
pub(crate) struct NormalizedRun {
    workflow_bundle: WorkflowBundle,
    entrypoint: ManifestPath,
    workflow: BundledWorkflow,
    workflow_layer: Option<SettingsLayer>,
    cwd: PathBuf,
    server_run_defaults: RunLayer,
    server_environment_defaults: MergeMap<EnvironmentLayer>,
    server_mcp_catalog: HashMap<String, McpServerSettings>,
    run_overrides: Option<RunLayer>,
    input_overrides: HashMap<String, toml::Value>,
    inline_goal_override: Option<String>,
    metadata: RunMetadata,
}

struct RunMetadata {
    run_id:              Option<RunId>,
    /// The environment the run overrides selected, for Petri's settings
    /// layer.
    environment_id:      Option<String>,
    storage_root:        PathBuf,
    workflow_slug:       Option<String>,
    workflow_version_id: Option<WorkflowVersionId>,
    target:              Option<RunTarget>,
    title:               Option<String>,
    automation:          Option<AutomationRef>,
    git:                 Option<GitContext>,
    parent_id:           Option<RunId>,
    provenance:          RunProvenance,
    web_url:             Option<String>,
}

/// Settings-layered output. Variable substitution is a separate stage so
/// callers can snapshot run variables after settings resolution and apply
/// the snapshot through [`apply_run_variables`].
pub(crate) struct LayeredRun {
    workflow_bundle: WorkflowBundle,
    entrypoint:      ManifestPath,
    workflow:        BundledWorkflow,
    settings:        WorkflowSettings,
    cwd:             PathBuf,
    metadata:        RunMetadata,
}

/// Variable-substituted stage output. Callers may inspect the resolved
/// settings before policy checks, then move it into [`compile_and_pin`].
pub(crate) struct PreparedRun {
    layered: LayeredRun,
    vars:    HashMap<String, String>,
}

impl PreparedRun {
    pub(crate) fn settings(&self) -> &WorkflowSettings {
        &self.layered.settings
    }

    /// The run-variable snapshot, for the engine's `{{ vars.* }}`.
    pub(crate) fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    /// The acquired bundle, for an engine that compiles it itself.
    pub(crate) fn workflow_bundle(&self) -> &WorkflowBundle {
        &self.layered.workflow_bundle
    }

    pub(crate) fn entrypoint(&self) -> &ManifestPath {
        &self.layered.entrypoint
    }

    pub(crate) fn target(&self) -> Option<&RunTarget> {
        self.layered.metadata.target.as_ref()
    }

    pub(crate) fn with_target_and_git(
        mut self,
        target: RunTarget,
        git: Option<GitContext>,
    ) -> Self {
        self.layered.metadata.target = Some(target);
        self.layered.metadata.git = git;
        self
    }

    pub(crate) fn parent_id(&self) -> Option<RunId> {
        self.layered.metadata.parent_id
    }

    /// The environment the run overrides selected, when they did.
    pub(crate) fn environment_id(&self) -> Option<&str> {
        self.layered.metadata.environment_id.as_deref()
    }

    pub(crate) fn resolve_run_id(mut self) -> (Self, RunId) {
        let run_id = self.layered.metadata.run_id.unwrap_or_default();
        self.layered.metadata.run_id = Some(run_id);
        (self, run_id)
    }

    pub(crate) fn with_web_url(mut self, web_url: Option<String>) -> Self {
        self.layered.metadata.web_url = web_url;
        self
    }
}

/// Model-pinned stage output ready for pure persistence-input assembly.
pub(crate) struct PinnedRun {
    materialized: MaterializedRun,
    metadata:     RunMetadata,
    /// What Petri admitted for the run.
    admission:    PetriAdmission,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunCompilerError {
    /// The acquired source bundle is invalid: missing entrypoint or broken
    /// bundled-file references.
    #[error(transparent)]
    InvalidSource(#[from] InvalidSourceError),

    /// A settings source or path is invalid, or the layered settings failed to
    /// resolve.
    #[error(transparent)]
    InvalidSettings(Box<InvalidSettingsError>),

    /// The run-variable snapshot could not be substituted into the resolved
    /// run settings.
    #[error("Run config variable interpolation failed: {0}")]
    VariableInterpolation(#[from] VariableInterpolationError),

    /// Graph compilation or model pinning failed in the workflow engine. The
    /// full [`WorkflowError`] is preserved so callers can distinguish
    /// validation, parse, and model-selection failures.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
}

// Shared source errors also serve manifest preview operations.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InvalidSourceError {
    #[error("manifest target path is missing from workflows map")]
    MissingEntrypoint { entrypoint: ManifestPath },

    #[error("unsupported dockerfile reference: {reference}")]
    UnsupportedDockerfileReference {
        config_path: ManifestPath,
        reference:   String,
    },

    #[error("missing bundled dockerfile: {dockerfile_path}")]
    MissingDockerfile {
        config_path:     ManifestPath,
        dockerfile_path: ManifestPath,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvalidSettingsError {
    #[error("Failed to parse run config TOML")]
    Parse {
        path:   ManifestPath,
        #[source]
        source: ParseError,
    },

    #[error("failed to resolve manifest settings")]
    Resolve {
        #[source]
        source: fabro_config::ResolveErrors,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum VariableInterpolationError {
    #[error(transparent)]
    Interpolation(#[from] ResolveError),

    #[error("run.artifacts.include[{index}]: {source}")]
    ArtifactGlob {
        index:  usize,
        #[source]
        source: WorkspaceGlobError,
    },
}

pub(crate) type Result<T> = std::result::Result<T, RunCompilerError>;

fn invalid_settings(source: InvalidSettingsError) -> RunCompilerError {
    RunCompilerError::InvalidSettings(Box::new(source))
}

/// Normalize the bundle entrypoint while retaining the admitted workflow
/// settings.
pub(crate) fn normalize_source(input: RawRunCompilerInput) -> Result<NormalizedRun> {
    let RawRunCompilerInput {
        workflow_bundle,
        entrypoint,
        cwd,
        server_run_defaults,
        server_environment_defaults,
        server_mcp_catalog,
        workflow_layer,
        run_overrides,
        input_overrides,
        inline_goal_override,
        run_id,
        title,
        parent_id,
        git,
        storage_root,
        workflow_slug,
        workflow_version_id,
        target,
        provenance,
        web_url,
        automation,
    } = input;
    let mut workflow = workflow_bundle
        .workflow(&entrypoint)
        .cloned()
        .ok_or_else(|| InvalidSourceError::MissingEntrypoint {
            entrypoint: entrypoint.clone(),
        })?;
    workflow.path = entrypoint.clone();
    let environment_id = run_overrides
        .as_ref()
        .and_then(|run| run.environment.as_ref())
        .and_then(|environment| environment.id.clone());

    Ok(NormalizedRun {
        workflow_bundle,
        entrypoint,
        workflow,
        workflow_layer,
        cwd,
        server_run_defaults,
        server_environment_defaults,
        server_mcp_catalog,
        run_overrides,
        input_overrides,
        inline_goal_override,
        metadata: RunMetadata {
            run_id,
            environment_id,
            storage_root,
            workflow_slug,
            workflow_version_id,
            target,
            title,
            automation,
            git,
            parent_id,
            provenance,
            web_url,
        },
    })
}

/// Layer settings from every configured source and apply the submitted input
/// and goal overrides.
pub(crate) fn layer_settings(normalized: NormalizedRun) -> Result<LayeredRun> {
    let NormalizedRun {
        workflow_bundle,
        entrypoint,
        workflow,
        workflow_layer,
        cwd,
        server_run_defaults,
        server_environment_defaults,
        server_mcp_catalog,
        run_overrides,
        input_overrides,
        inline_goal_override,
        metadata,
    } = normalized;
    let mut builder = WorkflowSettingsBuilder::new()
        .server_manifest_defaults(server_run_defaults, server_environment_defaults)
        .server_mcp_catalog(server_mcp_catalog);
    if let Some(run) = run_overrides {
        builder = builder.run_overrides(run);
    }
    if let Some(layer) = workflow_layer {
        builder = builder.workflow_layer(layer);
    }
    let mut settings = builder
        .build()
        .map_err(|source| invalid_settings(InvalidSettingsError::Resolve { source }))?;
    settings.run.inputs.extend(input_overrides);
    if let Some(goal) = inline_goal_override {
        settings.run.goal = Some(RunGoal::Inline(InterpString::parse(&goal)));
    }

    Ok(LayeredRun {
        workflow_bundle,
        entrypoint,
        workflow,
        settings,
        cwd,
        metadata,
    })
}

/// Apply a run-variable snapshot to the layered settings and validate the
/// resulting artifact globs. The snapshot is also retained for graph template
/// rendering during compilation.
pub(crate) fn apply_run_variables(
    mut layered: LayeredRun,
    vars: HashMap<String, String>,
) -> Result<PreparedRun> {
    substitute_run_variables(&vars, &mut layered.settings)?;
    Ok(PreparedRun { layered, vars })
}

/// Stages two and three for a run Petri admitted: parse the Fabro graph
/// the read side displays, with no lint and no model pinning, and record
/// the admission on the run.
pub(crate) async fn compile_admitted(
    prepared: PreparedRun,
    admission: PetriAdmission,
) -> Result<PinnedRun> {
    task::spawn_blocking(move || {
        let PreparedRun {
            layered:
                LayeredRun {
                    workflow_bundle,
                    entrypoint,
                    workflow,
                    settings,
                    cwd,
                    metadata,
                },
            vars,
        } = prepared;
        let compiled = operations::compile_admitted_run(CreateRunCompileInput {
            workflow: WorkflowInput::Bundled(workflow),
            settings,
            vars,
            cwd,
            workflow_path: Some(entrypoint),
            workflow_bundle: Some(workflow_bundle),
        })?;
        Ok(PinnedRun {
            materialized: operations::materialize_admitted_run(compiled),
            metadata,
            admission,
        })
    })
    .await
    .map_err(|source| {
        RunCompilerError::Workflow(WorkflowError::engine_with_source(
            "workflow create task failed",
            source,
        ))
    })?
}

/// Stage four: purely assemble the complete persistence input. Every durable
/// field — run id, captured definition, automation reference — is set here
/// once; nothing mutates the result afterwards.
pub(crate) fn assemble_run(pinned: PinnedRun) -> CreateRunPersistenceInput {
    let PinnedRun {
        materialized,
        metadata,
        admission,
    } = pinned;
    let RunMetadata {
        run_id,
        // Consumed at admission, as the launch's environment; the resolved
        // settings carry the environment the run persists.
        environment_id: _,
        storage_root,
        workflow_slug,
        workflow_version_id,
        target,
        title,
        automation,
        git,
        parent_id,
        provenance,
        web_url,
    } = metadata;
    operations::assemble_create_run_persistence_input(materialized, CreateRunPersistenceMetadata {
        run_id: run_id.expect("run ID should be resolved before compilation"),
        storage_root,
        workflow_slug,
        workflow_version_id,
        target,
        title,
        automation,
        git,
        fork_source_ref: None,
        parent_id,
        provenance,
        web_url,
        admission,
    })
}

/// Parse one bundle-relative settings source, rejecting keys that are not
/// allowed for `settings_source` and inlining dockerfile references from the
/// bundled files.
///
/// Parses via [`SettingsLayer`] so unknown nested keys (like a stale
/// `[server.integrations.github.permissions]` after the move to
/// `[run.integrations.github.permissions]`) trip `deny_unknown_fields`.
pub(crate) fn settings_layer_with_resolved_dockerfiles(
    source: &str,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
    settings_source: SettingsSource,
) -> Result<SettingsLayer> {
    let parse_error = |source| {
        invalid_settings(InvalidSettingsError::Parse {
            path: config_path.clone(),
            source,
        })
    };
    let mut layer = source.parse::<SettingsLayer>().map_err(parse_error)?;
    parse::validate_settings_source(&layer, settings_source).map_err(parse_error)?;
    resolve_dockerfiles(&mut layer, config_path, files)?;
    Ok(layer)
}

fn resolve_dockerfiles(
    layer: &mut SettingsLayer,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
) -> Result<()> {
    for image in layer.environment_images_mut() {
        resolve_dockerfile(image, config_path, files)?;
    }
    Ok(())
}

fn resolve_dockerfile(
    image: &mut EnvironmentImageLayer,
    config_path: &ManifestPath,
    files: &HashMap<ManifestPath, String>,
) -> Result<()> {
    let Some(EnvironmentDockerfileLayer::Path { path }) = image.dockerfile.as_ref() else {
        return Ok(());
    };
    let reference = path.clone();
    let dockerfile_path = ManifestPath::from_reference(config_path.parent_or_dot(), &reference)
        .ok_or_else(|| InvalidSourceError::UnsupportedDockerfileReference {
            config_path: config_path.clone(),
            reference:   reference.clone(),
        })?;
    let content = files.get(&dockerfile_path).cloned().ok_or_else(|| {
        InvalidSourceError::MissingDockerfile {
            config_path:     config_path.clone(),
            dockerfile_path: dockerfile_path.clone(),
        }
    })?;
    image.dockerfile = Some(EnvironmentDockerfileLayer::Inline(content));
    Ok(())
}

/// Substitute run-scoped variables into the resolved run settings, then
/// re-validate the artifact-include globs: a substituted variable can make a
/// previously-safe glob unsafe.
pub(crate) fn substitute_run_variables(
    variables: &HashMap<String, String>,
    settings: &mut WorkflowSettings,
) -> std::result::Result<(), VariableInterpolationError> {
    settings
        .run
        .substitute_variables(|name| variables.get(name).cloned())?;
    for (index, pattern) in settings.run.artifacts.include.iter().enumerate() {
        WorkspaceGlob::try_new(pattern)
            .map_err(|source| VariableInterpolationError::ArtifactGlob { index, source })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::error::Error as _;

    use fabro_config::EnvironmentDockerfileLayer;
    use fabro_types::settings::interp::ResolveCtx;
    use fabro_types::settings::run::RunGoal;
    use fabro_types::{AutomationRef, Principal, RunProvenance, SystemActorKind};
    use fabro_workflow::workflow_bundle::ParsedWorkflowConfig;

    use super::*;

    const DOT: &str = r#"digraph Test {
        graph [goal="Graph goal"]
        start [shape=Mdiamond]
        work [prompt="Ship {{ inputs.target }} for {{ vars.owner }}", model="gpt-5.4"]
        exit [shape=Msquare]
        start -> work -> exit
    }"#;

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("fixture manifest path should be valid")
    }

    fn provenance() -> RunProvenance {
        RunProvenance {
            server:  None,
            client:  None,
            subject: Principal::System {
                system_kind: SystemActorKind::Engine,
            },
        }
    }

    fn workflow(
        entrypoint: &ManifestPath,
        workflow_toml: Option<&str>,
        files: HashMap<ManifestPath, String>,
    ) -> BundledWorkflow {
        BundledWorkflow {
            path: entrypoint.clone(),
            source: DOT.to_string(),
            config: workflow_toml.map(|source| ParsedWorkflowConfig {
                path:   manifest_path("flows/workflow.toml"),
                source: source.to_string(),
            }),
            files,
        }
    }

    fn raw_input(
        workflow_toml: Option<&str>,
        files: HashMap<ManifestPath, String>,
    ) -> RawRunCompilerInput {
        let entrypoint = manifest_path("flows/workflow.fabro");
        let workflow = workflow(&entrypoint, workflow_toml, files);
        let workflow_layer = workflow.config.as_ref().map(|config| {
            settings_layer_with_resolved_dockerfiles(
                &config.source,
                &config.path,
                &workflow.files,
                SettingsSource::Workflow,
            )
            .expect("valid admitted workflow settings")
        });
        RawRunCompilerInput {
            workflow_bundle: WorkflowBundle::new(HashMap::from([(entrypoint.clone(), workflow)])),
            entrypoint,
            cwd: PathBuf::from("/workspace"),
            server_run_defaults: RunLayer::default(),
            server_environment_defaults: fabro_environment::seeded_catalog_layer(),
            server_mcp_catalog: HashMap::new(),
            workflow_layer,
            run_overrides: None,
            input_overrides: HashMap::new(),
            inline_goal_override: None,
            run_id: Some(RunId::new()),
            title: None,
            parent_id: None,
            git: None,
            storage_root: PathBuf::from("/tmp/fabro-storage"),
            workflow_slug: None,
            workflow_version_id: None,
            target: None,
            provenance: provenance(),
            web_url: None,
            automation: None,
        }
    }

    fn prepare_run(
        input: RawRunCompilerInput,
        vars: HashMap<String, String>,
    ) -> Result<PreparedRun> {
        apply_run_variables(layer_settings(normalize_source(input)?)?, vars)
    }

    #[test]
    fn normalize_source_rejects_missing_entrypoint() {
        let mut input = raw_input(None, HashMap::new());
        input.entrypoint = manifest_path("flows/missing.fabro");

        let Err(error) = normalize_source(input) else {
            panic!("missing entrypoint should fail");
        };

        assert!(matches!(
            error,
            RunCompilerError::InvalidSource(InvalidSourceError::MissingEntrypoint { .. })
        ));
    }

    #[test]
    fn settings_parser_rejects_missing_dockerfile_with_pinned_message() {
        let workflow_toml = r#"
_version = 1

[run.environment.image]
dockerfile = { path = "Dockerfile" }
"#;

        let Err(error) = settings_layer_with_resolved_dockerfiles(
            workflow_toml,
            &manifest_path("flows/workflow.toml"),
            &HashMap::new(),
            SettingsSource::Workflow,
        ) else {
            panic!("missing dockerfile should fail");
        };

        assert!(matches!(
            error,
            RunCompilerError::InvalidSource(InvalidSourceError::MissingDockerfile { .. })
        ));
        assert_eq!(
            error.to_string(),
            "missing bundled dockerfile: flows/Dockerfile"
        );
    }

    #[test]
    fn settings_parser_preserves_parse_source_chain() {
        let workflow_toml = r#"
_version = 1

[run.unknown-table]
key = "value"
"#;

        let Err(error) = settings_layer_with_resolved_dockerfiles(
            workflow_toml,
            &manifest_path("flows/workflow.toml"),
            &HashMap::new(),
            SettingsSource::Workflow,
        ) else {
            panic!("unknown settings key should fail");
        };

        assert_eq!(error.to_string(), "Failed to parse run config TOML");
        let source = error
            .source()
            .expect("parse error should retain the TOML source");
        assert!(source.to_string().contains("unknown"));
    }

    #[test]
    fn normalize_source_resolves_bundled_dockerfile() {
        let workflow_toml = r#"
_version = 1

[run.environment.image]
dockerfile = { path = "Dockerfile" }
"#;
        let normalized = normalize_source(raw_input(
            Some(workflow_toml),
            HashMap::from([(
                manifest_path("flows/Dockerfile"),
                "FROM ubuntu:24.04\n".to_string(),
            )]),
        ))
        .expect("bundled dockerfile should resolve");
        let dockerfile = normalized
            .workflow_layer
            .as_ref()
            .and_then(|layer| layer.run.as_ref())
            .and_then(|run| run.environment.as_ref())
            .and_then(|environment| environment.image.as_ref())
            .and_then(|image| image.dockerfile.as_ref());

        assert_eq!(
            dockerfile,
            Some(&EnvironmentDockerfileLayer::Inline(
                "FROM ubuntu:24.04\n".to_string()
            ))
        );
    }

    #[test]
    fn settings_apply_precedence_vars_inputs_and_safe_artifact_globs() {
        let workflow_toml = r#"
_version = 1

[run.metadata]
layer = "workflow"
owner = "{{ vars.owner }}"

[run.inputs]
target = "workflow"

[run.artifacts]
include = ["reports/{{ vars.owner }}/*.json"]
"#;
        let mut input = raw_input(Some(workflow_toml), HashMap::new());
        input.run_overrides = Some(
            toml::from_str::<SettingsLayer>(
                r#"
_version = 1

[run.metadata]
layer = "args"
owner = "{{ vars.owner }}"
"#,
            )
            .expect("args settings should parse")
            .run
            .expect("args run layer should exist"),
        );
        input.input_overrides.insert(
            "target".to_string(),
            toml::Value::String("override".to_string()),
        );
        input.inline_goal_override = Some("Ship {{ vars.owner }}".to_string());

        let prepared = prepare_run(
            input,
            HashMap::from([("owner".to_string(), "payments".to_string())]),
        )
        .expect("settings should prepare");
        let settings = prepared.settings();

        assert_eq!(
            settings.run.metadata.get("layer").map(String::as_str),
            Some("args")
        );
        assert_eq!(
            settings.run.metadata.get("owner").map(String::as_str),
            Some("payments")
        );
        assert_eq!(
            settings.run.inputs.get("target"),
            Some(&toml::Value::String("override".to_string()))
        );
        assert_eq!(settings.run.artifacts.include, vec![
            "reports/payments/*.json"
        ]);
        let Some(RunGoal::Inline(goal)) = settings.run.goal.as_ref() else {
            panic!("inline goal override should win");
        };
        assert_eq!(
            goal.resolve_with(&mut ResolveCtx::default()).unwrap(),
            "Ship payments"
        );
    }

    #[test]
    fn settings_reject_artifact_glob_made_unsafe_by_variable() {
        let workflow_toml = r#"
_version = 1

[run.artifacts]
include = ["reports/{{ vars.path }}/*.json"]
"#;
        let input = raw_input(Some(workflow_toml), HashMap::new());

        let Err(error) = prepare_run(
            input,
            HashMap::from([("path".to_string(), "../secrets".to_string())]),
        ) else {
            panic!("unsafe artifact glob should fail");
        };

        assert!(matches!(
            error,
            RunCompilerError::VariableInterpolation(VariableInterpolationError::ArtifactGlob {
                index:  0,
                source: WorkspaceGlobError::ParentTraversal { .. },
            })
        ));
    }

    #[tokio::test]
    async fn assembly_retains_entrypoint_and_run_metadata() {
        let run_id = RunId::new();
        let parent_id = RunId::new();
        let automation = AutomationRef {
            id:              "nightly".to_string(),
            name:            Some("Nightly".to_string()),
            trigger_id:      Some("schedule".to_string()),
            workflow_source: None,
        };
        let workflow_version_id = fabro_types::test_support::test_workflow_version_id();
        let mut input = raw_input(None, HashMap::new());
        input.run_id = Some(run_id);
        input.parent_id = Some(parent_id);
        input.title = Some("Compiler boundary".to_string());
        input.workflow_slug = Some("compiler-boundary".to_string());
        input.workflow_version_id = Some(workflow_version_id);
        input.web_url = Some(format!("https://fabro.test/runs/{run_id}"));
        input.automation = Some(automation.clone());
        input.input_overrides.insert(
            "target".to_string(),
            toml::Value::String("checkout".to_string()),
        );
        let expected_entrypoint = input.entrypoint.clone();

        let prepared = prepare_run(
            input,
            HashMap::from([("owner".to_string(), "payments".to_string())]),
        )
        .expect("settings should prepare");
        let pinned = compile_admitted(prepared, PetriAdmission::default())
            .await
            .expect("the admitted graph should compile");
        let persistence = assemble_run(pinned);

        assert_eq!(persistence.run_id(), run_id);
        assert_eq!(persistence.workflow_slug(), Some("compiler-boundary"));
        assert_eq!(persistence.workflow_version_id(), Some(workflow_version_id));
        assert_eq!(persistence.automation(), Some(&automation));
        assert_eq!(
            persistence
                .definition()
                .map(|definition| &definition.workflow_path),
            Some(&expected_entrypoint)
        );
        assert_eq!(
            persistence.materialized().settings().run.goal.as_ref(),
            Some(&RunGoal::Inline(InterpString::parse("Graph goal")))
        );
    }
}
