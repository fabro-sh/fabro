use chrono::{Local, Utc};
use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_model::{Catalog, Provider};
use fabro_sandbox::SandboxProvider;
use fabro_store::SlateStore;
use fabro_types::{RunId, Settings};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::FabroError;
use crate::pipeline::types::PersistOptions;
use crate::pipeline::{self, Persisted, TransformOptions, Validated};
use crate::records::RunRecord;
use crate::run_lookup::default_runs_base;
use crate::transforms::{Transform, expand_vars};
use fabro_sandbox::daytona::detect_repo_info;

use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};
use crate::event::{
    WorkflowRunEvent, append_workflow_event, canonicalize_event_at, normalize_json_value,
};

const RUN_CONFIG_FILE: &str = "workflow.toml";

#[derive(Clone, Debug)]
pub struct CreateRunInput {
    pub workflow: WorkflowInput,
    pub settings: Settings,
    pub cwd: PathBuf,
    pub workflow_slug: Option<String>,
    pub run_dir: Option<PathBuf>,
    pub run_id: Option<RunId>,
    pub host_repo_path: Option<String>,
    pub base_branch: Option<String>,
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
    run_dir: Option<PathBuf>,
    run_id: Option<RunId>,
    workflow_slug: Option<String>,
    labels: HashMap<String, String>,
    base_branch: Option<String>,
    working_directory: PathBuf,
    host_repo_path: Option<String>,
}

/// Resolve workflow inputs, normalize settings, and persist a run directory.
pub async fn create(store: &SlateStore, request: CreateRunInput) -> Result<CreatedRun, FabroError> {
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
        run_dir,
        run_id,
        host_repo_path,
        base_branch,
    } = request;

    let settings = resolved.settings.clone();
    let run_id = run_id.unwrap_or_else(RunId::new);
    let storage_dir = settings.storage_dir();
    let run_dir = run_dir.unwrap_or_else(|| {
        make_run_dir(
            &storage_dir.join("runs"),
            &run_id.to_string(),
            settings.dry_run_enabled(),
        )
    });
    let working_directory = resolved.working_directory.clone();
    let host_repo_path =
        host_repo_path.or_else(|| Some(working_directory.to_string_lossy().to_string()));
    let base_branch = base_branch.or_else(|| {
        detect_repo_info(&working_directory)
            .ok()
            .and_then(|(_, branch)| branch)
    });

    let goal_override = resolved.goal_override.clone();
    let base_dir = resolved.base_dir.clone();

    let persisted = create_from_source(
        &resolved.raw_source,
        PersistCreateOptions {
            settings,
            run_dir: Some(run_dir.clone()),
            run_id: Some(run_id),
            workflow_slug: workflow_slug.or(resolved.workflow_slug.clone()),
            labels: resolved.settings.labels.clone(),
            base_branch,
            working_directory,
            host_repo_path,
        },
        base_dir,
        goal_override.as_deref(),
    )?;

    write_run_config_snapshot(&run_dir, resolved.workflow_toml_path.as_deref())?;
    let workflow_config = resolved
        .workflow_toml_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    persist_created_run(store, &persisted, &resolved.raw_source, workflow_config).await?;

    Ok(CreatedRun {
        persisted,
        run_id,
        run_dir,
        dot_path: resolved.dot_path,
    })
}

async fn persist_created_run(
    store: &SlateStore,
    persisted: &Persisted,
    workflow_source: &str,
    workflow_config: Option<String>,
) -> Result<(), FabroError> {
    let record = persisted.run_record();
    let run_dir_string = persisted.run_dir().to_string_lossy().to_string();
    let run_store = match store
        .create_run(&record.run_id, record.created_at, Some(&run_dir_string))
        .await
    {
        Ok(run_store) => run_store,
        Err(err) => store
            .open_run(&record.run_id)
            .await
            .map_err(|open_err| FabroError::engine(open_err.to_string()))
            .or_else(|_| Err(FabroError::engine(err.to_string())))?,
    };

    let envelope = canonicalize_event_at(
        &record.run_id,
        &WorkflowRunEvent::RunCreated {
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
            base_branch: record.base_branch.clone(),
            workflow_slug: record.workflow_slug.clone(),
            db_prefix: None,
        },
        record.created_at,
    );
    let payload = fabro_store::EventPayload::new(
        serde_json::to_value(&envelope).map_err(|err| FabroError::engine(err.to_string()))?,
        &record.run_id,
    )
    .map_err(store_error)?;
    run_store
        .append_event(&payload)
        .await
        .map(|_| ())
        .map_err(store_error)?;
    append_workflow_event(
        run_store.as_ref(),
        &record.run_id,
        &WorkflowRunEvent::RunSubmitted { reason: None },
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

fn write_run_config_snapshot(
    run_dir: &Path,
    workflow_toml_path: Option<&Path>,
) -> Result<(), FabroError> {
    if let Some(toml_path) = workflow_toml_path {
        if toml_path.is_file() {
            std::fs::copy(toml_path, run_dir.join(RUN_CONFIG_FILE))
                .map_err(|err| FabroError::Io(err.to_string()))?;
        }
    }

    Ok(())
}

fn create_from_source(
    dot_source: &str,
    options: PersistCreateOptions,
    base_dir: Option<PathBuf>,
    goal_override: Option<&str>,
) -> Result<Persisted, FabroError> {
    let validated = preprocess_and_validate(
        dot_source,
        base_dir,
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
    base_dir: Option<PathBuf>,
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
            base_dir,
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
        run_dir,
        run_id,
        workflow_slug,
        labels,
        base_branch,
        working_directory,
        host_repo_path,
    } = options;

    let settings = resolve_run_settings(settings, validated.graph());

    let run_id = run_id.unwrap_or_else(RunId::new);
    let run_dir =
        run_dir.unwrap_or_else(|| default_run_dir(&run_id.to_string(), settings.dry_run_enabled()));

    let run_record = RunRecord {
        run_id,
        created_at: Utc::now(),
        settings,
        graph: validated.graph().clone(),
        workflow_slug,
        working_directory,
        host_repo_path,
        base_branch,
        labels,
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

pub(crate) fn default_run_dir(run_id: &str, dry_run: bool) -> PathBuf {
    make_run_dir(&default_runs_base(), run_id, dry_run)
}

pub(crate) fn make_run_dir(runs_base: &Path, run_id: &str, dry_run: bool) -> PathBuf {
    if dry_run {
        runs_base.join(format!(
            "{}-dry-run-{}",
            Local::now().format("%Y%m%d"),
            run_id
        ))
    } else {
        runs_base.join(format!("{}-{}", Local::now().format("%Y%m%d"), run_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::{SlateStore, StoreHandle};
    use fabro_types::fixtures;
    use object_store::local::LocalFileSystem;
    use object_store::memory::InMemory;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::operations::{ValidateInput, validate};
    fn memory_store() -> StoreHandle {
        Arc::new(SlateStore::new(
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
                run_dir: Some(dir.path().join("run")),
                run_id: None,
                host_repo_path: None,
                base_branch: None,
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
                run_dir: Some(dir.path().join("run")),
                run_id: Some(fixtures::RUN_1),
                host_repo_path: Some(dir.path().display().to_string()),
                base_branch: Some("main".to_string()),
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
        assert!(!created.run_dir.join("id.txt").exists());
    }

    #[tokio::test]
    async fn create_copies_workflow_toml_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflow");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(workflow_dir.join("workflow.fabro"), MINIMAL_DOT).unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();

        let store = memory_store();
        let created = create(
            &store,
            CreateRunInput {
                workflow: WorkflowInput::Path(workflow_dir.join("workflow.toml")),
                settings: Settings {
                    storage_dir: Some(dir.path().join("storage")),
                    dry_run: Some(true),
                    ..Default::default()
                },
                cwd: dir.path().to_path_buf(),
                workflow_slug: None,
                run_dir: None,
                run_id: None,
                host_repo_path: None,
                base_branch: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(created.run_dir.join("workflow.toml")).unwrap(),
            "version = 1\ngraph = \"workflow.fabro\"\n"
        );
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
                run_dir: Some(dir.path().join("run")),
                run_id: Some(fixtures::RUN_2),
                host_repo_path: None,
                base_branch: None,
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
    async fn create_hydrates_run_created_event_into_store() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(storage_dir.join("store")).unwrap();
        let object_store =
            Arc::new(LocalFileSystem::new_with_prefix(storage_dir.join("store")).unwrap());
        let store = StoreHandle::from(Arc::new(SlateStore::new(
            object_store,
            "",
            Duration::from_millis(1),
        )));
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
                run_dir: Some(run_dir.clone()),
                run_id: Some(fixtures::RUN_3),
                host_repo_path: None,
                base_branch: None,
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
}
