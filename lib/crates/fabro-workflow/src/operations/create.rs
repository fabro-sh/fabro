#![expect(
    clippy::disallowed_methods,
    reason = "sync workflow creation path: reads workflow.toml during workflow load and persists \
              .fabro scaffolding outside the Tokio execution hot path"
)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_config::Storage;
use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_model::{Catalog, ProviderId};
use fabro_store::Database;
use fabro_types::{
    AutomationRef, ForkSourceRef, GitContext, ManifestPath, RunId, RunProvenance, WorkflowSettings,
};
use fabro_util::json::normalize_json_value;
use tokio::task::spawn_blocking;

use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};
use crate::error::Error;
use crate::event::{Event, append_event, to_run_event_at};
use crate::file_resolver::FileResolver;
use crate::pipeline::types::PersistOptions;
use crate::pipeline::{self, Persisted, TransformOptions, Validated};
use crate::records::RunSpec;
use crate::run_lookup::default_scratch_base;
use crate::run_materialization::materialize_run;
use crate::transforms::{RenderMode, Transform};
use crate::workflow_bundle::{RunDefinition, WorkflowBundle};

#[derive(Clone, Debug)]
pub struct CreateRunInput {
    pub workflow: WorkflowInput,
    pub settings: WorkflowSettings,
    pub cwd: PathBuf,
    pub workflow_slug: Option<String>,
    pub workflow_path: Option<ManifestPath>,
    pub workflow_bundle: Option<WorkflowBundle>,
    pub submitted_manifest_bytes: Option<Vec<u8>>,
    pub run_id: Option<RunId>,
    pub title: Option<String>,
    pub automation: Option<AutomationRef>,
    pub git: Option<GitContext>,
    pub fork_source_ref: Option<ForkSourceRef>,
    pub parent_id: Option<RunId>,
    pub provenance: Option<RunProvenance>,
    pub configured_providers: Vec<ProviderId>,
    /// Public URL where this run can be viewed in the web UI, when the server
    /// has the web UI enabled. Recorded on the `run.created` event so attach
    /// replays can surface the link.
    pub web_url: Option<String>,
}

#[derive(Debug)]
pub struct CreatedRun {
    pub persisted: Persisted,
    pub run_id:    RunId,
    pub run_dir:   PathBuf,
    pub dot_path:  Option<PathBuf>,
}

struct PersistCreateOptions {
    settings:             WorkflowSettings,
    run_id:               Option<RunId>,
    run_dir:              Option<PathBuf>,
    workflow_slug:        Option<String>,
    source_name:          Option<String>,
    labels:               HashMap<String, String>,
    source_directory:     Option<String>,
    automation:           Option<AutomationRef>,
    git:                  Option<GitContext>,
    fork_source_ref:      Option<ForkSourceRef>,
    provenance:           Option<RunProvenance>,
    configured_providers: Vec<ProviderId>,
    catalog:              Arc<Catalog>,
}

/// Resolve workflow inputs, normalize settings using the caller-provided
/// catalog, and persist a run directory.
pub async fn create(
    store: &Database,
    request: CreateRunInput,
    storage_root: PathBuf,
    catalog: Arc<Catalog>,
) -> Result<CreatedRun, Error> {
    let resolved = resolve_workflow(ResolveWorkflowInput {
        workflow: request.workflow,
        settings: request.settings,
        cwd:      request.cwd,
    })
    .map_err(|err| Error::Parse(err.to_string()))?;
    let labels = resolved.settings.combined_labels();
    let settings = resolved.settings.clone();

    let CreateRunInput {
        workflow: _,
        settings: _,
        cwd: _,
        workflow_slug,
        workflow_path,
        workflow_bundle,
        submitted_manifest_bytes,
        run_id,
        title,
        automation,
        git,
        fork_source_ref,
        parent_id,
        provenance,
        configured_providers,
        web_url,
    } = request;

    let run_id = run_id.unwrap_or_else(RunId::new);
    let storage = Storage::new(storage_root);
    let run_dir = storage.run_scratch(&run_id).root().to_path_buf();
    let source_directory = Some(resolved.working_directory.to_string_lossy().to_string());

    let goal_override = resolved.goal_override.clone();
    let current_dir = resolved.current_dir.clone();
    let file_resolver = resolved.file_resolver.clone();
    let resolved_workflow_slug = resolved.workflow_slug.clone();
    let persisted_run_dir = run_dir.clone();
    let accepted_definition = match (&workflow_path, &workflow_bundle) {
        (Some(workflow_path), Some(workflow_bundle)) => Some(RunDefinition::new(
            workflow_path.clone(),
            workflow_bundle.clone(),
        )),
        _ => None,
    };

    let raw_source = resolved.raw_source.clone();
    let source_name = resolved
        .dot_path
        .as_ref()
        .map(|path| path.display().to_string());
    let persisted = spawn_blocking(move || {
        create_from_source(
            &raw_source,
            PersistCreateOptions {
                settings,
                run_id: Some(run_id),
                run_dir: Some(persisted_run_dir),
                workflow_slug: workflow_slug.or(resolved_workflow_slug),
                source_name,
                labels,
                source_directory,
                automation,
                git,
                fork_source_ref,
                provenance,
                configured_providers,
                catalog,
            },
            current_dir,
            file_resolver,
            goal_override.as_deref(),
        )
    })
    .await
    .map_err(|err| Error::engine_with_source("workflow create task failed", err))??;

    let workflow_config = resolved
        .workflow_toml_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    persist_created_run(
        store,
        &persisted,
        &resolved.raw_source,
        workflow_config,
        submitted_manifest_bytes.as_deref(),
        accepted_definition.as_ref(),
        title,
        parent_id,
        web_url,
    )
    .await?;

    Ok(CreatedRun {
        persisted,
        run_id,
        run_dir,
        dot_path: resolved.dot_path,
    })
}

async fn persist_created_run(
    store: &Database,
    persisted: &Persisted,
    workflow_source: &str,
    workflow_config: Option<String>,
    submitted_manifest_bytes: Option<&[u8]>,
    accepted_definition: Option<&RunDefinition>,
    explicit_title: Option<String>,
    parent_id: Option<RunId>,
    web_url: Option<String>,
) -> Result<(), Error> {
    let record = persisted.run_spec();
    let run_store = match store.create_run(&record.run_id).await {
        Ok(run_store) => run_store,
        Err(err) => store
            .open_run(&record.run_id)
            .await
            .map_err(|open_err| Error::engine(open_err.to_string()))
            .map_err(|_| Error::engine(err.to_string()))?,
    };
    let manifest_blob = match submitted_manifest_bytes {
        Some(bytes) => Some(run_store.write_blob(bytes).await.map_err(store_error)?),
        None => None,
    };
    let definition_blob = match accepted_definition {
        Some(definition) => {
            let bytes =
                serde_json::to_vec(definition).map_err(|err| Error::engine(err.to_string()))?;
            Some(run_store.write_blob(&bytes).await.map_err(store_error)?)
        }
        None => None,
    };

    let title = explicit_title.unwrap_or_else(|| fabro_types::infer_run_title(record.graph.goal()));
    let stored = to_run_event_at(
        &record.run_id,
        &Event::RunCreated {
            run_id: record.run_id,
            title: Some(title),
            settings: normalize_json_value(
                serde_json::to_value(&record.settings)
                    .map_err(|err| Error::engine(err.to_string()))?,
            ),
            graph: normalize_json_value(
                serde_json::to_value(&record.graph)
                    .map_err(|err| Error::engine(err.to_string()))?,
            ),
            workflow_source: (!workflow_source.is_empty()).then(|| workflow_source.to_string()),
            workflow_config,
            labels: record
                .labels
                .clone()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            run_dir: persisted.run_dir().display().to_string(),
            source_directory: record.source_directory.clone(),
            workflow_slug: record.workflow_slug.clone(),
            automation: record.automation.clone(),
            db_prefix: None,
            provenance: record.provenance.clone(),
            manifest_blob,
            git: record.git.clone(),
            fork_source_ref: record.fork_source_ref.clone(),
            retried_from: None,
            parent_id,
            web_url,
        },
        record.run_id.created_at(),
        None,
    );
    let payload = fabro_store::EventPayload::new(
        serde_json::to_value(&stored).map_err(|err| Error::engine(err.to_string()))?,
        &record.run_id,
    )
    .map_err(store_error)?;
    run_store
        .append_event(&payload)
        .await
        .map(|_| ())
        .map_err(store_error)?;
    append_event(&run_store, &record.run_id, &Event::RunSubmitted {
        definition_blob,
    })
    .await
    .map_err(store_error)
}

fn store_error(err: impl std::fmt::Display) -> Error {
    Error::engine(err.to_string())
}

fn create_from_source(
    dot_source: &str,
    options: PersistCreateOptions,
    current_dir: Option<PathBuf>,
    file_resolver: Option<Arc<dyn FileResolver>>,
    goal_override: Option<&str>,
) -> Result<Persisted, Error> {
    let mut validated = preprocess_and_validate(
        dot_source,
        options.source_name.clone(),
        current_dir,
        file_resolver,
        Vec::new(),
        Some(&options.settings),
        goal_override,
        RenderMode::Structural,
        &options.catalog,
    )?;

    validated.promote_template_undefined_variables_to_errors();
    if validated.has_errors() {
        return Err(Error::ValidationFailed {
            diagnostics: validated.diagnostics().to_vec(),
        });
    }

    persist_validated(validated, options)
}

pub(super) fn preprocess_and_validate(
    dot_source: &str,
    source_name: Option<String>,
    current_dir: Option<PathBuf>,
    file_resolver: Option<Arc<dyn FileResolver>>,
    custom_transforms: Vec<Box<dyn Transform>>,
    settings: Option<&WorkflowSettings>,
    goal_override: Option<&str>,
    render_mode: RenderMode,
    catalog: &Arc<Catalog>,
) -> Result<Validated, Error> {
    let inputs = run_inputs(settings);
    let mut parsed = pipeline::parse(dot_source)?;
    apply_goal_override(&mut parsed.graph, goal_override);

    let transformed = pipeline::transform(parsed, &TransformOptions {
        current_dir,
        file_resolver,
        inputs,
        source_name,
        render_mode,
        custom_transforms,
        catalog: Arc::clone(catalog),
    })?;
    Ok(pipeline::validate(transformed, catalog.as_ref(), &[]))
}

fn run_inputs(settings: Option<&WorkflowSettings>) -> HashMap<String, toml::Value> {
    settings
        .map(|settings| settings.run.inputs.clone())
        .unwrap_or_default()
}

fn apply_goal_override(graph: &mut Graph, goal_override: Option<&str>) {
    if let Some(goal_override) = goal_override {
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String(goal_override.to_string()),
        );
    }
}

fn persist_validated(
    validated: Validated,
    options: PersistCreateOptions,
) -> Result<Persisted, Error> {
    let PersistCreateOptions {
        settings,
        run_id,
        run_dir,
        workflow_slug,
        source_name: _,
        labels,
        source_directory,
        automation,
        git,
        fork_source_ref,
        provenance,
        configured_providers,
        catalog,
    } = options;

    let settings = materialize_run(
        settings,
        validated.graph(),
        catalog.as_ref(),
        &configured_providers,
    );

    let run_id = run_id.unwrap_or_else(RunId::new);
    let run_dir = run_dir.unwrap_or_else(|| default_run_dir(&run_id));

    let run_spec = RunSpec {
        run_id,
        settings,
        graph: validated.graph().clone(),
        graph_source: Some(validated.source().to_string()),
        workflow_slug,
        automation,
        source_directory,
        labels,
        provenance,
        manifest_blob: None,
        definition_blob: None,
        git,
        fork_source_ref,
    };

    pipeline::persist(validated, PersistOptions { run_dir, run_spec })
}

pub(crate) fn default_run_dir(run_id: &RunId) -> PathBuf {
    make_run_dir(&default_scratch_base(), run_id)
}

pub fn make_run_dir(scratch_base: &Path, run_id: &RunId) -> PathBuf {
    fabro_config::RunScratch::for_run(scratch_base, run_id)
        .root()
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{Local, TimeZone, Utc};
    use fabro_config::{
        ReplaceMap, RunExecutionLayer, RunGoalLayer, RunLayer, RunModelLayer, RunPullRequestLayer,
        WorkflowSettingsBuilder,
    };
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::Database;
    use fabro_types::settings::InterpString;
    use fabro_types::settings::run::RunMode;
    use fabro_types::{WorkflowSettings, fixtures};
    use fabro_util::error::collect_chain;
    use fabro_validate::Severity;
    use object_store::local::LocalFileSystem;
    use object_store::memory::InMemory;

    use super::*;
    use crate::operations::{ValidateInput, validate};
    use crate::pipeline::types::TEMPLATE_UNDEFINED_VARIABLE_RULE;
    use crate::workflow_bundle::BundledWorkflow;
    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    fn settings_from_run_layer(run: RunLayer) -> WorkflowSettings {
        WorkflowSettingsBuilder::new()
            .server_manifest_defaults(
                RunLayer::default(),
                fabro_environment::seeded_catalog_layer(),
            )
            .run_overrides(run)
            .build()
            .expect("settings should resolve")
    }

    fn test_default_settings() -> WorkflowSettings {
        WorkflowSettingsBuilder::new()
            .server_manifest_defaults(
                RunLayer::default(),
                fabro_environment::seeded_catalog_layer(),
            )
            .build()
            .expect("default settings should resolve")
    }

    fn test_catalog() -> Arc<Catalog> {
        Arc::new(Catalog::from_builtin().unwrap())
    }

    fn validate_dot(dot_source: &str, settings: WorkflowSettings) -> Validated {
        validate(ValidateInput {
            workflow: WorkflowInput::DotSource {
                source:   dot_source.to_string(),
                base_dir: None,
            },
            settings,
            cwd: PathBuf::from("."),
            custom_transforms: Vec::new(),
            catalog: test_catalog(),
        })
        .unwrap()
    }

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    #[test]
    fn validate_minimal() {
        let validated = validate_dot(MINIMAL_DOT, WorkflowSettings::default());
        validated.raise_on_errors().unwrap();

        assert_eq!(validated.graph().name, "Test");
        assert!(validated.graph().find_start_node().is_some());
        assert!(validated.graph().find_exit_node().is_some());
    }

    #[test]
    fn validate_with_unbound_inputs_warns_but_succeeds() {
        let dot = r#"digraph Test {
            graph [goal="Build feature"]
            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare,  label="Exit"]
            work  [label="Work", prompt="Work on {{ inputs.app_dir }}"]
            start -> work -> exit
        }"#;
        let validated = validate_dot(dot, WorkflowSettings::default());
        validated.raise_on_errors().unwrap();

        let diagnostic = validated
            .diagnostics()
            .iter()
            .find(|d| d.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE)
            .expect("expected a template_undefined_variable diagnostic");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert!(
            diagnostic.message.contains("inputs.app_dir"),
            "missing variable in: {}",
            diagnostic.message
        );
    }

    #[test]
    fn promote_template_undefined_rule_turns_warning_into_error() {
        let dot = r#"digraph Test {
            graph [goal="Build {{ inputs.app_dir }}"]
            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare,  label="Exit"]
            start -> exit
        }"#;
        let mut validated = validate_dot(dot, WorkflowSettings::default());
        assert!(!validated.has_errors());

        validated.promote_template_undefined_variables_to_errors();

        assert!(validated.has_errors());
        let diagnostic = validated
            .diagnostics()
            .iter()
            .find(|d| d.rule == TEMPLATE_UNDEFINED_VARIABLE_RULE)
            .expect("expected template diagnostic");
        assert_eq!(diagnostic.severity, Severity::Error);
    }

    #[test]
    fn strict_template_error_for_inline_prompt_names_workflow_file_and_node() {
        let dot = r#"digraph ValidatePlan {
            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare, label="Exit"]
            test_inline_prompt [label="moo" prompt="{{ inputs.foo }}"]
            start -> test_inline_prompt -> exit
        }"#;

        let result = preprocess_and_validate(
            dot,
            Some("workflow.fabro".to_string()),
            Some(PathBuf::from(".")),
            None,
            Vec::new(),
            Some(&WorkflowSettings::default()),
            None,
            RenderMode::Strict,
            &test_catalog(),
        );
        let Err(err) = result else {
            panic!("expected strict mode to hard-fail on unbound inline prompt");
        };

        let rendered = collect_chain(&err).join(": ");
        assert!(rendered.contains("workflow.fabro"), "{rendered}");
        assert!(rendered.contains("test_inline_prompt"), "{rendered}");
        assert!(rendered.contains("prompt"), "{rendered}");
        assert!(!rendered.contains("<string>"), "{rendered}");
    }

    #[test]
    fn imported_prompt_template_error_names_prompt_file_and_node() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_path = dir.path().join("test.md");
        std::fs::write(&prompt_path, "{{ inputs.foo }}").unwrap();
        let dot = r#"digraph ValidatePlan {
            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare, label="Exit"]
            test_imported_prompt [label="moo" prompt="@test.md"]
            start -> test_imported_prompt -> exit
        }"#;

        let result = preprocess_and_validate(
            dot,
            Some("workflow.fabro".to_string()),
            Some(dir.path().to_path_buf()),
            Some(Arc::new(crate::file_resolver::FilesystemFileResolver::new(
                None,
            ))),
            Vec::new(),
            Some(&WorkflowSettings::default()),
            None,
            RenderMode::Strict,
            &test_catalog(),
        );
        let Err(err) = result else {
            panic!("expected strict mode to hard-fail on unbound imported prompt");
        };

        let rendered = collect_chain(&err).join(": ");
        assert!(rendered.contains("test.md"), "{rendered}");
        assert!(rendered.contains("test_imported_prompt"), "{rendered}");
        assert!(rendered.contains("prompt"), "{rendered}");
        assert!(!rendered.contains("<string>"), "{rendered}");
    }

    #[test]
    fn validate_applies_variable_expansion() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Goal: {{ goal }}"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = validate_dot(dot, WorkflowSettings::default());
        validated.raise_on_errors().unwrap();

        let prompt = validated.graph().nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: Fix bugs");
    }

    #[test]
    fn validate_does_not_render_source_level_templated_node_ids() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            {{ inputs.step }} [prompt="Do work"]
            exit [shape=Msquare]
            start -> exit
        }"#;

        let result = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   dot.to_string(),
                base_dir: None,
            },
            settings:          settings_from_run_layer({
                let mut inputs = std::collections::HashMap::new();
                inputs.insert("step".to_string(), toml::Value::String("work".to_string()));
                RunLayer {
                    inputs: Some(inputs),
                    ..RunLayer::default()
                }
            }),
            cwd:               PathBuf::from("."),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        });

        assert!(result.is_err());
    }

    #[test]
    fn inline_and_file_prompt_diagnostics_match() {
        fn normalized_diagnostics(
            validated: &Validated,
        ) -> Vec<(String, Severity, String, Option<String>)> {
            validated
                .diagnostics()
                .iter()
                .map(|diagnostic| {
                    (
                        diagnostic.rule.clone(),
                        diagnostic.severity.clone(),
                        diagnostic.message.clone(),
                        diagnostic.node_id.clone(),
                    )
                })
                .collect()
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("missing.md"),
            "Work in {{ inputs.app_dir }}",
        )
        .unwrap();
        std::fs::write(dir.path().join("goal.md"), "Goal: {{ goal }}").unwrap();

        let inline_missing = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   r#"digraph Test {
                    graph [goal="Demo"]
                    start [shape=Mdiamond]
                    work [prompt="Work in {{ inputs.app_dir }}"]
                    exit [shape=Msquare]
                    start -> work -> exit
                }"#
                .to_string(),
                base_dir: Some(dir.path().to_path_buf()),
            },
            settings:          WorkflowSettings::default(),
            cwd:               dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();
        let file_missing = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   r#"digraph Test {
                    graph [goal="Demo"]
                    start [shape=Mdiamond]
                    work [prompt="@missing.md"]
                    exit [shape=Msquare]
                    start -> work -> exit
                }"#
                .to_string(),
                base_dir: Some(dir.path().to_path_buf()),
            },
            settings:          WorkflowSettings::default(),
            cwd:               dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();
        assert_eq!(
            normalized_diagnostics(&inline_missing),
            normalized_diagnostics(&file_missing)
        );

        let inline_goal = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   r#"digraph Test {
                    graph [goal="Ship"]
                    start [shape=Mdiamond]
                    work [prompt="Goal: {{ goal }}"]
                    exit [shape=Msquare]
                    start -> work -> exit
                }"#
                .to_string(),
                base_dir: Some(dir.path().to_path_buf()),
            },
            settings:          WorkflowSettings::default(),
            cwd:               dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();
        let file_goal = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   r#"digraph Test {
                    graph [goal="Ship"]
                    start [shape=Mdiamond]
                    work [prompt="@goal.md"]
                    exit [shape=Msquare]
                    start -> work -> exit
                }"#
                .to_string(),
                base_dir: Some(dir.path().to_path_buf()),
            },
            settings:          WorkflowSettings::default(),
            cwd:               dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();
        assert_eq!(
            inline_goal.graph().nodes["work"].attrs.get("prompt"),
            file_goal.graph().nodes["work"].attrs.get("prompt")
        );
        assert_eq!(
            normalized_diagnostics(&inline_goal),
            normalized_diagnostics(&file_goal)
        );
    }

    #[test]
    fn make_run_dir_uses_run_id_timestamp_in_local_time() {
        let scratch_base = Path::new("/tmp/scratch");
        let run_id = RunId::from(ulid::Ulid::from_datetime(
            Utc.with_ymd_and_hms(2026, 3, 27, 12, 0, 0).unwrap().into(),
        ));
        let expected_date = run_id
            .created_at()
            .with_timezone(&Local)
            .format("%Y%m%d")
            .to_string();

        assert_eq!(
            make_run_dir(scratch_base, &run_id),
            scratch_base.join(format!("{expected_date}-{run_id}"))
        );
    }

    #[test]
    fn validate_applies_stylesheet() {
        let dot = r#"digraph Test {
            graph [goal="Test", model_stylesheet="* { model: sonnet; }"]
            start [shape=Mdiamond]
            work  [label="Work"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = validate_dot(dot, WorkflowSettings::default());
        validated.raise_on_errors().unwrap();

        assert_eq!(
            validated.graph().nodes["work"].attrs.get("model"),
            Some(&AttrValue::String("claude-sonnet-4-6".into()))
        );
    }

    #[test]
    fn validate_applies_config_vars_and_goal_override() {
        let dot = r#"digraph Test {
            graph [goal="original"]
            start [shape=Mdiamond]
            work [prompt="{{ inputs.who }}: {{ goal }}"]
            exit [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = validate_dot(
            dot,
            settings_from_run_layer({
                let mut inputs = std::collections::HashMap::new();
                inputs.insert("who".to_string(), toml::Value::String("agent".to_string()));
                RunLayer {
                    goal: Some(RunGoalLayer::Inline(InterpString::parse("override"))),
                    inputs: Some(inputs),
                    ..RunLayer::default()
                }
            }),
        );
        validated.raise_on_errors().unwrap();

        assert_eq!(validated.graph().goal(), "override");
        let prompt = validated.graph().nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "agent: override");
    }

    #[test]
    fn validate_returns_error_on_invalid_dot() {
        let result = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   "not a graph".to_string(),
                base_dir: None,
            },
            settings:          WorkflowSettings::default(),
            cwd:               PathBuf::from("."),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        });
        assert!(result.is_err());
    }

    #[test]
    fn validate_returns_validation_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let validated = validate_dot(dot, WorkflowSettings::default());

        assert!(validated.has_errors());
        assert!(validated.raise_on_errors().is_err());
    }

    #[test]
    fn validate_supports_custom_transforms() {
        struct TagTransform;

        impl Transform for TagTransform {
            fn apply(
                &self,
                graph: fabro_graphviz::graph::Graph,
            ) -> Result<fabro_graphviz::graph::Graph, Error> {
                let mut graph = graph;
                for node in graph.nodes.values_mut() {
                    node.attrs
                        .insert("tagged".to_string(), AttrValue::Boolean(true));
                }

                Ok(graph)
            }
        }

        let validated = validate(ValidateInput {
            workflow:          WorkflowInput::DotSource {
                source:   MINIMAL_DOT.to_string(),
                base_dir: None,
            },
            settings:          WorkflowSettings::default(),
            cwd:               PathBuf::from("."),
            custom_transforms: vec![Box::new(TagTransform)],
            catalog:           test_catalog(),
        })
        .unwrap();
        validated.raise_on_errors().unwrap();

        assert_eq!(
            validated.graph().nodes["start"].attrs.get("tagged"),
            Some(&AttrValue::Boolean(true))
        );
    }

    #[test]
    fn validate_from_file_uses_parent_directory_for_inlining() {
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("goal.txt");
        let dot_path = dir.path().join("workflow.fabro");

        std::fs::write(&data_path, "ship it").unwrap();
        std::fs::write(
            &dot_path,
            r#"digraph Test {
                graph [goal="@goal.txt"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }"#,
        )
        .unwrap();

        let validated = validate(ValidateInput {
            workflow:          WorkflowInput::Path(dot_path),
            settings:          WorkflowSettings::default(),
            cwd:               dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();
        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "ship it");
    }

    #[test]
    fn validate_from_file_resolves_minijinja_includes_relative_to_prompt_and_goal_files() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_dir = dir.path().join("prompts");
        let goal_dir = dir.path().join("goals");
        std::fs::create_dir_all(&prompt_dir).unwrap();
        std::fs::create_dir_all(&goal_dir).unwrap();
        std::fs::write(
            prompt_dir.join("prompt.md"),
            r#"{% include "prompt.tpl.md" %}"#,
        )
        .unwrap();
        std::fs::write(prompt_dir.join("prompt.tpl.md"), "included prompt").unwrap();
        std::fs::write(goal_dir.join("goal.md"), r#"{% include "goal.tpl.md" %}"#).unwrap();
        std::fs::write(goal_dir.join("goal.tpl.md"), "included goal").unwrap();

        let dot_path = dir.path().join("workflow.fabro");
        std::fs::write(
            &dot_path,
            r#"digraph Test {
                graph [goal="@goals/goal.md"]
                start [shape=Mdiamond]
                work [prompt="@prompts/prompt.md"]
                exit [shape=Msquare]
                start -> work -> exit
            }"#,
        )
        .unwrap();

        let validated = validate(ValidateInput {
            workflow:          WorkflowInput::Path(dot_path),
            settings:          WorkflowSettings::default(),
            cwd:               dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();

        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "included goal");
        assert_eq!(
            validated.graph().nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("included prompt")
        );
    }

    #[test]
    fn validate_from_bundle_resolves_nested_import_files_relative_to_imported_graph() {
        let validated = validate(ValidateInput {
            workflow:          WorkflowInput::Bundled(BundledWorkflow {
                path:   ManifestPath::from_wire("workflow.fabro").unwrap(),
                source: r#"digraph Test {
                    graph [goal="Ship"]
                    start [shape=Mdiamond]
                    validate [import="./child/validate.fabro"]
                    exit [shape=Msquare]
                    start -> validate -> exit
                }"#
                .to_string(),
                config: None,
                files:  HashMap::from([
                    (
                        ManifestPath::from_wire("child/validate.fabro").unwrap(),
                        r#"digraph Validate {
                            start [shape=Mdiamond]
                            lint [prompt="@../prompts/lint.md"]
                            exit [shape=Msquare]
                            start -> lint -> exit
                        }"#
                        .to_string(),
                    ),
                    (
                        ManifestPath::from_wire("prompts/lint.md").unwrap(),
                        "Lint {{ goal }}".to_string(),
                    ),
                ]),
            }),
            settings:          WorkflowSettings::default(),
            cwd:               PathBuf::from("."),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();

        validated.raise_on_errors().unwrap();
        assert_eq!(
            validated.graph().nodes["validate.lint"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Lint Ship")
        );
    }

    #[test]
    fn validate_from_bundle_resolves_minijinja_includes_in_prompt_and_goal_files() {
        let validated = validate(ValidateInput {
            workflow:          WorkflowInput::Bundled(BundledWorkflow {
                path:   ManifestPath::from_wire("workflow.fabro").unwrap(),
                source: r#"digraph Test {
                    graph [goal="@goals/goal.md"]
                    start [shape=Mdiamond]
                    work [prompt="@prompts/work.md"]
                    exit [shape=Msquare]
                    start -> work -> exit
                }"#
                .to_string(),
                config: None,
                files:  HashMap::from([
                    (
                        ManifestPath::from_wire("goals/goal.md").unwrap(),
                        r#"{% include "goal.tpl.md" %}"#.to_string(),
                    ),
                    (
                        ManifestPath::from_wire("goals/goal.tpl.md").unwrap(),
                        "Bundled goal".to_string(),
                    ),
                    (
                        ManifestPath::from_wire("prompts/work.md").unwrap(),
                        r#"{% include "work.tpl.md" %}"#.to_string(),
                    ),
                    (
                        ManifestPath::from_wire("prompts/work.tpl.md").unwrap(),
                        "Bundled prompt".to_string(),
                    ),
                ]),
            }),
            settings:          WorkflowSettings::default(),
            cwd:               PathBuf::from("."),
            custom_transforms: Vec::new(),
            catalog:           test_catalog(),
        })
        .unwrap();

        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "Bundled goal");
        assert_eq!(
            validated.graph().nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Bundled prompt")
        );
    }

    #[tokio::test]
    async fn create_returns_validation_failed_with_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let dir = tempfile::tempdir().unwrap();
        let storage_root = dir.path().join("storage");
        let store = memory_store();
        let err = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source:   dot.to_string(),
                    base_dir: None,
                },
                settings: test_default_settings(),
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: None,
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_root,
            test_catalog(),
        )
        .await
        .unwrap_err();

        match err {
            Error::ValidationFailed { diagnostics } => {
                assert!(!diagnostics.is_empty());
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test asserts the raw template source"
    )]
    #[tokio::test]
    async fn create_persists_normalized_config_and_initial_state() {
        let dir = tempfile::tempdir().unwrap();
        let storage_root = dir.path().join("storage");
        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source:   MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: settings_from_run_layer({
                    let mut metadata = HashMap::new();
                    metadata.insert("env".to_string(), "test".to_string());
                    RunLayer {
                        goal: Some(RunGoalLayer::Inline(InterpString::parse("override goal"))),
                        metadata: ReplaceMap::from(metadata),
                        model: Some(RunModelLayer {
                            name: Some(InterpString::parse("sonnet")),
                            ..RunModelLayer::default()
                        }),
                        pull_request: Some(RunPullRequestLayer {
                            enabled: Some(false),
                            ..RunPullRequestLayer::default()
                        }),
                        execution: Some(RunExecutionLayer {
                            mode: Some(RunMode::DryRun),
                            ..RunExecutionLayer::default()
                        }),
                        ..RunLayer::default()
                    }
                }),
                cwd: dir.path().to_path_buf(),
                workflow_slug: Some("slug".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                title: None,
                automation: None,
                git: Some(fabro_types::GitContext {
                    origin_url:   String::new(),
                    branch:       "main".to_string(),
                    sha:          None,
                    dirty:        fabro_types::DirtyStatus::Clean,
                    push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
                }),
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_root.clone(),
            test_catalog(),
        )
        .await
        .unwrap();

        assert_eq!(created.run_id, fixtures::RUN_1);
        assert_eq!(created.persisted.run_spec().graph.goal(), "override goal");
        assert_eq!(
            created
                .persisted
                .run_spec()
                .settings
                .run
                .model
                .name
                .as_ref()
                .map(fabro_types::settings::InterpString::as_source)
                .as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            created
                .persisted
                .run_spec()
                .settings
                .run
                .model
                .provider
                .as_ref()
                .map(fabro_types::settings::InterpString::as_source)
                .as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            match &created.persisted.run_spec().settings.run.goal {
                Some(fabro_types::settings::run::RunGoal::Inline(value)) => {
                    Some(value.as_source())
                }
                _ => None,
            }
            .as_deref(),
            Some("override goal")
        );
        assert!(
            created
                .persisted
                .run_spec()
                .settings
                .run
                .pull_request
                .is_none()
        );
        assert_eq!(
            created.persisted.run_spec().workflow_slug.as_deref(),
            Some("slug")
        );
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        assert_eq!(
            run_store.state().await.unwrap().status,
            crate::run_status::RunStatus::Submitted
        );
        assert_eq!(
            created.run_dir,
            Storage::new(&storage_root)
                .run_scratch(&fixtures::RUN_1)
                .root()
                .to_path_buf()
        );
        assert!(created.run_dir.is_dir());
    }

    #[tokio::test]
    async fn create_persists_submitter_source_directory_from_request_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let storage_root = dir.path().join("storage");

        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source:   MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: settings_from_run_layer({
                    RunLayer {
                        working_dir: Some(InterpString::parse("workspace")),
                        execution: Some(RunExecutionLayer {
                            mode: Some(RunMode::DryRun),
                            ..RunExecutionLayer::default()
                        }),
                        ..RunLayer::default()
                    }
                }),
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_2),
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_root,
            test_catalog(),
        )
        .await
        .unwrap();

        assert_eq!(
            created.persisted.run_spec().source_directory.as_deref(),
            Some(workspace.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn create_persists_repo_origin_url_from_request() {
        let dir = tempfile::tempdir().unwrap();
        let storage_root = dir.path().join("storage");
        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source:   MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: dry_run_only_settings(),
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_2),
                title: None,
                automation: None,
                git: Some(fabro_types::GitContext {
                    origin_url:   "https://github.com/acme/widgets".to_string(),
                    branch:       String::new(),
                    sha:          None,
                    dirty:        fabro_types::DirtyStatus::Clean,
                    push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
                }),
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_root,
            test_catalog(),
        )
        .await
        .unwrap();

        assert_eq!(
            created.persisted.run_spec().repo_origin_url(),
            Some("https://github.com/acme/widgets")
        );
    }

    fn dry_run_only_settings() -> WorkflowSettings {
        settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        })
    }

    fn dry_run_with_storage(_storage_dir: &Path) -> WorkflowSettings {
        settings_from_run_layer(RunLayer {
            execution: Some(RunExecutionLayer {
                mode: Some(RunMode::DryRun),
                ..RunExecutionLayer::default()
            }),
            ..RunLayer::default()
        })
    }

    #[tokio::test]
    async fn create_hydrates_run_created_event_into_store() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        std::fs::create_dir_all(storage_dir.join("store")).unwrap();
        let object_store =
            Arc::new(LocalFileSystem::new_with_prefix(storage_dir.join("store")).unwrap());
        let store = Arc::new(Database::new(
            object_store,
            "",
            Duration::from_millis(1),
            None,
        ));
        let automation = fabro_types::AutomationRef {
            id:         "nightly".to_string(),
            name:       Some("Nightly".to_string()),
            trigger_id: Some("schedule_1".to_string()),
        };
        let created = create(
            store.as_ref(),
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source:   MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: dry_run_with_storage(&storage_dir),
                cwd: dir.path().to_path_buf(),
                workflow_slug: Some("slug".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_3),
                title: None,
                automation: Some(automation.clone()),
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: None,
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_dir.clone(),
            test_catalog(),
        )
        .await
        .unwrap();
        let run_store = store.open_run_reader(&created.run_id).await.unwrap();
        let events = run_store.list_events().await.unwrap();
        let state = run_store.state().await.unwrap();

        assert_eq!(events.first().unwrap().event.event_name(), "run.created");
        assert_eq!(
            created.persisted.run_spec().automation,
            Some(automation.clone())
        );
        assert_eq!(state.spec.automation, Some(automation));
    }

    #[tokio::test]
    async fn create_hydrates_provenance_into_store_state() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        std::fs::create_dir_all(storage_dir.join("store")).unwrap();
        let object_store =
            Arc::new(LocalFileSystem::new_with_prefix(storage_dir.join("store")).unwrap());
        let store = Arc::new(Database::new(
            object_store,
            "",
            Duration::from_millis(1),
            None,
        ));
        let created = create(
            store.as_ref(),
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source:   MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: dry_run_with_storage(&storage_dir),
                cwd: dir.path().to_path_buf(),
                workflow_slug: Some("slug".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_64),
                title: None,
                automation: None,
                git: None,
                fork_source_ref: None,
                parent_id: None,
                provenance: Some(fabro_types::RunProvenance {
                    server:  Some(fabro_types::RunServerProvenance {
                        version: "0.9.0".to_string(),
                    }),
                    client:  Some(fabro_types::RunClientProvenance {
                        user_agent: Some("fabro-cli/0.9.0".to_string()),
                        name:       Some("fabro-cli".to_string()),
                        version:    Some("0.9.0".to_string()),
                    }),
                    subject: Some(fabro_types::Principal::user(
                        fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
                        "octocat".to_string(),
                        fabro_types::AuthMethod::Github,
                    )),
                }),
                configured_providers: Vec::new(),
                web_url: None,
            },
            storage_dir,
            test_catalog(),
        )
        .await
        .unwrap();

        let run_store = store.open_run_reader(&created.run_id).await.unwrap();
        let state = run_store.state().await.unwrap();
        let run = state.spec;
        let provenance = run.provenance.expect("provenance should be projected");

        assert_eq!(provenance.server.unwrap().version, "0.9.0");
        assert_eq!(
            provenance.client.unwrap().name.as_deref(),
            Some("fabro-cli")
        );
        assert_eq!(
            provenance.subject.unwrap(),
            fabro_types::Principal::user(
                fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
                "octocat".to_string(),
                fabro_types::AuthMethod::Github,
            )
        );
    }
}
