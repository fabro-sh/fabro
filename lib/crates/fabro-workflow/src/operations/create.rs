use fabro_config::Storage;
use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_model::{Catalog, Provider};
use fabro_sandbox::SandboxProvider;
use fabro_store::Database;
use fabro_types::{RunId, RunProvenance, Settings};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::FabroError;
use crate::file_resolver::FileResolver;
use crate::pipeline::types::PersistOptions;
use crate::pipeline::{self, Persisted, TransformOptions, Validated};
use crate::records::RunRecord;
use crate::run_lookup::default_scratch_base;
use crate::transforms::{Transform, expand_vars};
use crate::workflow_bundle::{RunDefinition, WorkflowBundle};
use fabro_sandbox::daytona::detect_repo_info;
use fabro_util::json::normalize_json_value;

use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};
use crate::event::{Event, append_event, to_run_event_at};

#[derive(Clone, Debug)]
pub struct CreateRunInput {
    pub workflow: WorkflowInput,
    pub settings: Settings,
    pub cwd: PathBuf,
    pub workflow_slug: Option<String>,
    pub workflow_path: Option<PathBuf>,
    pub workflow_bundle: Option<WorkflowBundle>,
    pub submitted_manifest_bytes: Option<Vec<u8>>,
    pub run_id: Option<RunId>,
    pub host_repo_path: Option<String>,
    pub repo_origin_url: Option<String>,
    pub base_branch: Option<String>,
    pub provenance: Option<RunProvenance>,
}

#[derive(Debug)]
pub struct CreatedRun {
    pub persisted: Persisted,
    pub run_id: RunId,
    pub run_dir: PathBuf,
    pub dot_path: Option<PathBuf>,
}

struct PersistCreateOptions {
    settings: Settings,
    run_id: Option<RunId>,
    run_dir: Option<PathBuf>,
    workflow_slug: Option<String>,
    labels: HashMap<String, String>,
    base_branch: Option<String>,
    working_directory: PathBuf,
    host_repo_path: Option<String>,
    repo_origin_url: Option<String>,
    provenance: Option<RunProvenance>,
}

/// Resolve workflow inputs, normalize settings, and persist a run directory.
pub async fn create(store: &Database, request: CreateRunInput) -> Result<CreatedRun, FabroError> {
    let resolved = resolve_workflow(ResolveWorkflowInput {
        workflow: request.workflow,
        settings: request.settings,
        cwd: request.cwd,
    })
    .map_err(|err| FabroError::Parse(err.to_string()))?;

    if !resolved.settings.dry_run_enabled() {
        validate_sandbox_provider(&resolved.settings)?;
    }

    let CreateRunInput {
        workflow: _,
        settings: _,
        cwd: _,
        workflow_slug,
        workflow_path,
        workflow_bundle,
        submitted_manifest_bytes,
        run_id,
        host_repo_path,
        repo_origin_url,
        base_branch,
        provenance,
    } = request;

    let settings = resolved.settings.clone();
    let run_id = run_id.unwrap_or_else(RunId::new);
    let storage = Storage::new(settings.storage_dir());
    let run_dir = storage.run_scratch(&run_id).root().to_path_buf();
    let working_directory = resolved.working_directory.clone();
    let host_repo_path =
        host_repo_path.or_else(|| Some(working_directory.to_string_lossy().to_string()));
    let detected_repo = detect_repo_info(&working_directory).ok();
    let repo_origin_url = repo_origin_url.or_else(|| {
        detected_repo
            .as_ref()
            .map(|(origin_url, _)| fabro_github::normalize_repo_origin_url(origin_url))
    });
    let base_branch = base_branch.or_else(|| {
        detected_repo
            .as_ref()
            .and_then(|(_, branch)| branch.clone())
    });

    let goal_override = resolved.goal_override.clone();
    let current_dir = resolved.current_dir.clone();
    let file_resolver = resolved.file_resolver.clone();
    let accepted_definition = match (&workflow_path, &workflow_bundle) {
        (Some(workflow_path), Some(workflow_bundle)) => Some(RunDefinition::new(
            workflow_path.clone(),
            workflow_bundle.clone(),
        )),
        _ => None,
    };

    let persisted = create_from_source(
        &resolved.raw_source,
        PersistCreateOptions {
            settings,
            run_id: Some(run_id),
            run_dir: Some(run_dir.clone()),
            workflow_slug: workflow_slug.or(resolved.workflow_slug.clone()),
            labels: resolved.settings.labels.clone(),
            base_branch,
            working_directory,
            host_repo_path,
            repo_origin_url,
            provenance,
        },
        current_dir,
        file_resolver,
        goal_override.as_deref(),
    )?;

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
) -> Result<(), FabroError> {
    let record = persisted.run_record();
    let run_store = match store.create_run(&record.run_id).await {
        Ok(run_store) => run_store,
        Err(err) => store
            .open_run(&record.run_id)
            .await
            .map_err(|open_err| FabroError::engine(open_err.to_string()))
            .map_err(|_| FabroError::engine(err.to_string()))?,
    };
    let manifest_blob = match submitted_manifest_bytes {
        Some(bytes) => Some(run_store.write_blob(bytes).await.map_err(store_error)?),
        None => None,
    };
    let definition_blob = match accepted_definition {
        Some(definition) => {
            let bytes = serde_json::to_vec(definition)
                .map_err(|err| FabroError::engine(err.to_string()))?;
            Some(run_store.write_blob(&bytes).await.map_err(store_error)?)
        }
        None => None,
    };

    let stored = to_run_event_at(
        &record.run_id,
        &Event::RunCreated {
            run_id: record.run_id,
            settings: normalize_json_value(
                serde_json::to_value(&record.settings)
                    .map_err(|err| FabroError::engine(err.to_string()))?,
            ),
            graph: normalize_json_value(
                serde_json::to_value(&record.graph)
                    .map_err(|err| FabroError::engine(err.to_string()))?,
            ),
            workflow_source: (!workflow_source.is_empty()).then(|| workflow_source.to_string()),
            workflow_config,
            labels: record
                .labels
                .clone()
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
            run_dir: persisted.run_dir().display().to_string(),
            working_directory: record.working_directory.display().to_string(),
            host_repo_path: record.host_repo_path.clone(),
            repo_origin_url: record.repo_origin_url.clone(),
            base_branch: record.base_branch.clone(),
            workflow_slug: record.workflow_slug.clone(),
            db_prefix: None,
            provenance: record.provenance.clone(),
            manifest_blob,
        },
        record.run_id.created_at(),
    );
    let payload = fabro_store::EventPayload::new(
        serde_json::to_value(&stored).map_err(|err| FabroError::engine(err.to_string()))?,
        &record.run_id,
    )
    .map_err(store_error)?;
    run_store
        .append_event(&payload)
        .await
        .map(|_| ())
        .map_err(store_error)?;
    append_event(
        &run_store,
        &record.run_id,
        &Event::RunSubmitted {
            reason: None,
            definition_blob,
        },
    )
    .await
    .map_err(store_error)
}

fn store_error(err: impl std::fmt::Display) -> FabroError {
    FabroError::engine(err.to_string())
}

fn validate_sandbox_provider(settings: &Settings) -> Result<(), FabroError> {
    if let Some(provider) = settings
        .sandbox_settings()
        .and_then(|sandbox| sandbox.provider.as_deref())
    {
        provider
            .parse::<SandboxProvider>()
            .map_err(|err| FabroError::Precondition(format!("Invalid sandbox provider: {err}")))?;
    }

    Ok(())
}

fn create_from_source(
    dot_source: &str,
    options: PersistCreateOptions,
    current_dir: Option<PathBuf>,
    file_resolver: Option<Arc<dyn FileResolver>>,
    goal_override: Option<&str>,
) -> Result<Persisted, FabroError> {
    let validated = preprocess_and_validate(
        dot_source,
        current_dir,
        file_resolver,
        Vec::new(),
        Some(&options.settings),
        goal_override,
    )?;

    if validated.has_errors() {
        return Err(FabroError::ValidationFailed {
            diagnostics: validated.diagnostics().to_vec(),
        });
    }

    persist_validated(validated, options)
}

pub(super) fn preprocess_and_validate(
    dot_source: &str,
    current_dir: Option<PathBuf>,
    file_resolver: Option<Arc<dyn FileResolver>>,
    custom_transforms: Vec<Box<dyn Transform>>,
    settings: Option<&Settings>,
    goal_override: Option<&str>,
) -> Result<Validated, FabroError> {
    let source = match settings.and_then(|resolved| resolved.vars.as_ref()) {
        Some(vars) => {
            let mut vars = vars.clone();
            vars.insert("goal".to_string(), "$goal".to_string());
            expand_vars(dot_source, &vars)
                .map_err(|e| FabroError::Parse(format!("var expansion failed: {e}")))?
        }
        None => dot_source.to_string(),
    };

    let mut parsed = pipeline::parse(&source)?;
    apply_goal_override(&mut parsed.graph, goal_override);

    let transformed = pipeline::transform(
        parsed,
        &TransformOptions {
            current_dir,
            file_resolver,
            custom_transforms,
        },
    );
    Ok(pipeline::validate(transformed, &[]))
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
) -> Result<Persisted, FabroError> {
    let PersistCreateOptions {
        settings,
        run_id,
        run_dir,
        workflow_slug,
        labels,
        base_branch,
        working_directory,
        host_repo_path,
        repo_origin_url,
        provenance,
    } = options;

    let settings = resolve_run_settings(settings, validated.graph());

    let run_id = run_id.unwrap_or_else(RunId::new);
    let run_dir = run_dir.unwrap_or_else(|| default_run_dir(&run_id));

    let run_record = RunRecord {
        run_id,
        settings,
        graph: validated.graph().clone(),
        workflow_slug,
        working_directory,
        host_repo_path,
        repo_origin_url,
        base_branch,
        labels,
        provenance,
        manifest_blob: None,
        definition_blob: None,
    };

    pipeline::persist(
        validated,
        PersistOptions {
            run_dir,
            run_record,
        },
    )
}

pub(crate) fn resolve_run_settings(mut settings: Settings, graph: &Graph) -> Settings {
    let llm_settings = settings.llm.as_ref();
    let configured_model = llm_settings.and_then(|l| l.model.as_deref());
    let configured_provider = llm_settings.and_then(|l| l.provider.as_deref());
    let graph_provider = graph.attrs.get("default_provider").and_then(|v| v.as_str());
    let graph_model = graph.attrs.get("default_model").and_then(|v| v.as_str());

    let provider = configured_provider.or(graph_provider).map(str::to_string);

    let model = configured_model.or(graph_model).map_or_else(
        || {
            let catalog = Catalog::builtin();
            provider
                .as_deref()
                .and_then(|value| value.parse::<Provider>().ok())
                .and_then(|provider| catalog.default_for_provider(provider))
                .unwrap_or_else(|| catalog.default_from_env())
                .id
                .clone()
        },
        str::to_string,
    );

    let (resolved_model, resolved_provider) = match Catalog::builtin().get(&model) {
        Some(info) => (
            info.id.clone(),
            provider.or(Some(info.provider.to_string())),
        ),
        None => (model, provider),
    };

    let llm = settings.llm.get_or_insert_default();
    llm.model = Some(resolved_model);
    llm.provider = resolved_provider;

    let goal = graph.goal().to_string();
    settings.goal = if goal.is_empty() { None } else { Some(goal) };
    settings.pull_request = settings
        .pull_request
        .take()
        .filter(|pull_request| pull_request.enabled);

    settings
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
    use super::*;
    use chrono::{Local, TimeZone, Utc};
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::Database;
    use fabro_types::fixtures;
    use object_store::local::LocalFileSystem;
    use object_store::memory::InMemory;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::operations::{ValidateInput, validate};
    use crate::workflow_bundle::BundledWorkflow;
    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    fn validate_dot(dot_source: &str, settings: Settings) -> Validated {
        validate(ValidateInput {
            workflow: WorkflowInput::DotSource {
                source: dot_source.to_string(),
                base_dir: None,
            },
            settings,
            cwd: PathBuf::from("."),
            custom_transforms: Vec::new(),
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
        let validated = validate_dot(MINIMAL_DOT, Settings::default());
        validated.raise_on_errors().unwrap();

        assert_eq!(validated.graph().name, "Test");
        assert!(validated.graph().find_start_node().is_some());
        assert!(validated.graph().find_exit_node().is_some());
    }

    #[test]
    fn validate_applies_variable_expansion() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Goal: $goal"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = validate_dot(dot, Settings::default());
        validated.raise_on_errors().unwrap();

        let prompt = validated.graph().nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: Fix bugs");
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
        let validated = validate_dot(dot, Settings::default());
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
            work [prompt="$who: $goal"]
            exit [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = validate_dot(
            dot,
            Settings {
                vars: Some(HashMap::from([("who".to_string(), "agent".to_string())])),
                goal: Some("override".to_string()),
                ..Default::default()
            },
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
            workflow: WorkflowInput::DotSource {
                source: "not a graph".to_string(),
                base_dir: None,
            },
            settings: Settings::default(),
            cwd: PathBuf::from("."),
            custom_transforms: Vec::new(),
        });
        assert!(result.is_err());
    }

    #[test]
    fn validate_returns_validation_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let validated = validate_dot(dot, Settings::default());

        assert!(validated.has_errors());
        assert!(validated.raise_on_errors().is_err());
    }

    #[test]
    fn validate_supports_custom_transforms() {
        struct TagTransform;

        impl Transform for TagTransform {
            fn apply(&self, graph: fabro_graphviz::graph::Graph) -> fabro_graphviz::graph::Graph {
                let mut graph = graph;
                for node in graph.nodes.values_mut() {
                    node.attrs
                        .insert("tagged".to_string(), AttrValue::Boolean(true));
                }

                graph
            }
        }

        let validated = validate(ValidateInput {
            workflow: WorkflowInput::DotSource {
                source: MINIMAL_DOT.to_string(),
                base_dir: None,
            },
            settings: Settings::default(),
            cwd: PathBuf::from("."),
            custom_transforms: vec![Box::new(TagTransform)],
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
            workflow: WorkflowInput::Path(dot_path),
            settings: Settings::default(),
            cwd: dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
        })
        .unwrap();
        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "ship it");
    }

    #[test]
    fn validate_from_bundle_resolves_nested_import_files_relative_to_imported_graph() {
        let validated = validate(ValidateInput {
            workflow: WorkflowInput::Bundled(BundledWorkflow {
                logical_path: PathBuf::from("workflow.fabro"),
                source: r#"digraph Test {
                    graph [goal="Ship"]
                    start [shape=Mdiamond]
                    validate [import="./child/validate.fabro"]
                    exit [shape=Msquare]
                    start -> validate -> exit
                }"#
                .to_string(),
                files: HashMap::from([
                    (
                        PathBuf::from("child/validate.fabro"),
                        r#"digraph Validate {
                            start [shape=Mdiamond]
                            lint [prompt="@../prompts/lint.md"]
                            exit [shape=Msquare]
                            start -> lint -> exit
                        }"#
                        .to_string(),
                    ),
                    (PathBuf::from("prompts/lint.md"), "Lint $goal".to_string()),
                ]),
            }),
            settings: Settings::default(),
            cwd: PathBuf::from("."),
            custom_transforms: Vec::new(),
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

    #[tokio::test]
    async fn create_returns_validation_failed_with_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let dir = tempfile::tempdir().unwrap();
        let store = memory_store();
        let err = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source: dot.to_string(),
                    base_dir: None,
                },
                settings: Settings::default(),
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: None,
                host_repo_path: None,
                repo_origin_url: None,
                base_branch: None,
                provenance: None,
            },
        )
        .await
        .unwrap_err();

        match err {
            FabroError::ValidationFailed { diagnostics } => {
                assert!(!diagnostics.is_empty());
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_persists_normalized_config_and_initial_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source: MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: Settings {
                    llm: Some(fabro_config::run::LlmSettings {
                        model: Some("sonnet".to_string()),
                        provider: None,
                        fallbacks: None,
                    }),
                    pull_request: Some(fabro_config::run::PullRequestSettings {
                        enabled: false,
                        ..Default::default()
                    }),
                    goal: Some("override goal".to_string()),
                    dry_run: Some(true),
                    labels: HashMap::from([("env".to_string(), "test".to_string())]),
                    ..Default::default()
                },
                cwd: dir.path().to_path_buf(),
                workflow_slug: Some("slug".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_1),
                host_repo_path: Some(dir.path().display().to_string()),
                repo_origin_url: None,
                base_branch: Some("main".to_string()),
                provenance: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(created.run_id, fixtures::RUN_1);
        assert_eq!(created.persisted.run_record().graph.goal(), "override goal");
        assert_eq!(
            created
                .persisted
                .run_record()
                .settings
                .llm
                .as_ref()
                .and_then(|llm| llm.model.as_deref()),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            created
                .persisted
                .run_record()
                .settings
                .llm
                .as_ref()
                .and_then(|llm| llm.provider.as_deref()),
            Some("anthropic")
        );
        assert_eq!(
            created.persisted.run_record().settings.goal.as_deref(),
            Some("override goal")
        );
        assert!(
            created
                .persisted
                .run_record()
                .settings
                .pull_request
                .is_none()
        );
        assert_eq!(
            created.persisted.run_record().workflow_slug.as_deref(),
            Some("slug")
        );
        let run_store = store.open_run(&fixtures::RUN_1).await.unwrap();
        assert_eq!(
            run_store.state().await.unwrap().status.unwrap().status,
            crate::run_status::RunStatus::Submitted
        );
        assert_eq!(created.run_dir, default_run_dir(&fixtures::RUN_1));
        assert!(created.run_dir.is_dir());
    }

    #[tokio::test]
    async fn create_resolves_working_directory_and_repo_path_from_request_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source: MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: Settings {
                    work_dir: Some("workspace".to_string()),
                    dry_run: Some(true),
                    ..Default::default()
                },
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_2),
                host_repo_path: None,
                repo_origin_url: None,
                base_branch: None,
                provenance: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(created.persisted.run_record().working_directory, workspace);
        assert_eq!(
            created.persisted.run_record().host_repo_path.as_deref(),
            Some(
                created
                    .persisted
                    .run_record()
                    .working_directory
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[tokio::test]
    async fn create_persists_repo_origin_url_from_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source: MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: Settings {
                    dry_run: Some(true),
                    ..Default::default()
                },
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_2),
                host_repo_path: None,
                repo_origin_url: Some("https://github.com/acme/widgets".to_string()),
                base_branch: None,
                provenance: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            created.persisted.run_record().repo_origin_url.as_deref(),
            Some("https://github.com/acme/widgets")
        );
    }

    #[tokio::test]
    async fn create_hydrates_run_created_event_into_store() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        std::fs::create_dir_all(storage_dir.join("store")).unwrap();
        let object_store =
            Arc::new(LocalFileSystem::new_with_prefix(storage_dir.join("store")).unwrap());
        let store = Arc::new(Database::new(object_store, "", Duration::from_millis(1)));
        let created = create(
            store.as_ref(),
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source: MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: Settings {
                    storage_dir: Some(storage_dir.clone()),
                    dry_run: Some(true),
                    ..Default::default()
                },
                cwd: dir.path().to_path_buf(),
                workflow_slug: Some("slug".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_3),
                host_repo_path: None,
                repo_origin_url: None,
                base_branch: None,
                provenance: None,
            },
        )
        .await
        .unwrap();
        let run_store = store.open_run_reader(&created.run_id).await.unwrap();
        let events = run_store.list_events().await.unwrap();

        assert_eq!(
            events.first().unwrap().payload.as_value()["event"],
            "run.created"
        );
    }

    #[tokio::test]
    async fn create_hydrates_provenance_into_store_state() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        std::fs::create_dir_all(storage_dir.join("store")).unwrap();
        let object_store =
            Arc::new(LocalFileSystem::new_with_prefix(storage_dir.join("store")).unwrap());
        let store = Arc::new(Database::new(object_store, "", Duration::from_millis(1)));
        let created = create(
            store.as_ref(),
            CreateRunInput {
                workflow: WorkflowInput::DotSource {
                    source: MINIMAL_DOT.to_string(),
                    base_dir: None,
                },
                settings: Settings {
                    storage_dir: Some(storage_dir.clone()),
                    dry_run: Some(true),
                    ..Default::default()
                },
                cwd: dir.path().to_path_buf(),
                workflow_slug: Some("slug".to_string()),
                workflow_path: None,
                workflow_bundle: None,
                submitted_manifest_bytes: None,
                run_id: Some(fixtures::RUN_64),
                host_repo_path: None,
                repo_origin_url: None,
                base_branch: None,
                provenance: Some(fabro_types::RunProvenance {
                    server: Some(fabro_types::RunServerProvenance {
                        version: "0.9.0".to_string(),
                    }),
                    client: Some(fabro_types::RunClientProvenance {
                        user_agent: Some("fabro-cli/0.9.0".to_string()),
                        name: Some("fabro-cli".to_string()),
                        version: Some("0.9.0".to_string()),
                    }),
                    subject: Some(fabro_types::RunSubjectProvenance {
                        login: None,
                        auth_method: fabro_types::RunAuthMethod::Disabled,
                    }),
                }),
            },
        )
        .await
        .unwrap();

        let run_store = store.open_run_reader(&created.run_id).await.unwrap();
        let state = run_store.state().await.unwrap();
        let run = state.run.expect("run should be projected");
        let provenance = run.provenance.expect("provenance should be projected");

        assert_eq!(provenance.server.unwrap().version, "0.9.0");
        assert_eq!(
            provenance.client.unwrap().name.as_deref(),
            Some("fabro-cli")
        );
        assert_eq!(
            provenance.subject.unwrap().auth_method,
            fabro_types::RunAuthMethod::Disabled
        );
    }
}
