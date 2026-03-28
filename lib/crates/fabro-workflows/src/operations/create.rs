use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{Local, Utc};
use fabro_config::{FabroSettings, FabroSettingsExt};
use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_model::{Catalog, Provider};
use fabro_sandbox::SandboxProvider;

use crate::error::FabroError;
use crate::pipeline::types::PersistOptions;
use crate::pipeline::{self, Persisted, TransformOptions, Validated};
use crate::records::RunRecord;
use crate::run_lookup::default_runs_base;
use crate::run_status::{RunStatus, write_run_status};
use crate::transforms::{Transform, expand_vars};
use fabro_sandbox::daytona::detect_repo_info;

use super::source::{ResolveWorkflowInput, WorkflowInput, resolve_workflow};

const RUN_CONFIG_FILE: &str = "workflow.toml";

#[derive(Clone, Debug)]
pub struct CreateRunInput {
    pub workflow: WorkflowInput,
    pub settings: FabroSettings,
    pub cwd: PathBuf,
    pub workflow_slug: Option<String>,
    pub run_dir: Option<PathBuf>,
    pub run_id: Option<String>,
    pub host_repo_path: Option<String>,
    pub base_branch: Option<String>,
}

#[derive(Debug)]
pub struct CreatedRun {
    pub persisted: Persisted,
    pub run_id: String,
    pub run_dir: PathBuf,
    pub dot_path: Option<PathBuf>,
}

struct PersistCreateOptions {
    settings: FabroSettings,
    run_dir: Option<PathBuf>,
    run_id: Option<String>,
    workflow_slug: Option<String>,
    labels: HashMap<String, String>,
    base_branch: Option<String>,
    working_directory: PathBuf,
    host_repo_path: Option<String>,
}

/// Resolve workflow inputs, normalize settings, and persist a run directory.
pub fn create(request: CreateRunInput) -> Result<CreatedRun, FabroError> {
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
    let run_id = run_id.unwrap_or_else(|| ulid::Ulid::new().to_string());
    let storage_dir = settings.storage_dir();
    let run_dir = run_dir.unwrap_or_else(|| {
        make_run_dir(
            &storage_dir.join("runs"),
            &run_id,
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
            run_id: Some(run_id.clone()),
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
    write_run_status(&run_dir, RunStatus::Submitted, None);

    Ok(CreatedRun {
        persisted,
        run_id,
        run_dir,
        dot_path: resolved.dot_path,
    })
}

fn validate_sandbox_provider(settings: &FabroSettings) -> Result<(), FabroError> {
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
    settings: Option<&FabroSettings>,
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

    let run_id = run_id.unwrap_or_else(|| ulid::Ulid::new().to_string());
    let run_dir = run_dir.unwrap_or_else(|| default_run_dir(&run_id, settings.dry_run_enabled()));

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

pub(crate) fn resolve_run_settings(mut settings: FabroSettings, graph: &Graph) -> FabroSettings {
    let llm_config = settings.llm.as_ref();
    let configured_model = llm_config.and_then(|l| l.model.as_deref());
    let configured_provider = llm_config.and_then(|l| l.provider.as_deref());
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

    use crate::operations::{ValidateInput, validate};
    use crate::run_status::RunStatusRecordExt;

    fn validate_dot(dot_source: &str, settings: FabroSettings) -> Validated {
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
        let validated = validate_dot(MINIMAL_DOT, FabroSettings::default());
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
        let validated = validate_dot(dot, FabroSettings::default());
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
        let validated = validate_dot(dot, FabroSettings::default());
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
            FabroSettings {
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
            settings: FabroSettings::default(),
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
        let validated = validate_dot(dot, FabroSettings::default());

        assert!(validated.has_errors());
        assert!(validated.raise_on_errors().is_err());
    }

    #[test]
    fn validate_supports_custom_transforms() {
        struct TagTransform;

        impl Transform for TagTransform {
            fn apply(&self, graph: &mut fabro_graphviz::graph::Graph) {
                for node in graph.nodes.values_mut() {
                    node.attrs
                        .insert("tagged".to_string(), AttrValue::Boolean(true));
                }
            }
        }

        let validated = validate(ValidateInput {
            workflow: WorkflowInput::DotSource {
                source: MINIMAL_DOT.to_string(),
                base_dir: None,
            },
            settings: FabroSettings::default(),
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
            settings: FabroSettings::default(),
            cwd: dir.path().to_path_buf(),
            custom_transforms: Vec::new(),
        })
        .unwrap();
        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "ship it");
    }

    #[test]
    fn create_returns_validation_failed_with_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let dir = tempfile::tempdir().unwrap();
        let err = create(CreateRunInput {
            workflow: WorkflowInput::DotSource {
                source: dot.to_string(),
                base_dir: None,
            },
            settings: FabroSettings::default(),
            cwd: dir.path().to_path_buf(),
            workflow_slug: None,
            run_dir: Some(dir.path().join("run")),
            run_id: None,
            host_repo_path: None,
            base_branch: None,
        })
        .unwrap_err();

        match err {
            FabroError::ValidationFailed { diagnostics } => {
                assert!(!diagnostics.is_empty());
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn create_persists_normalized_config_and_initial_state() {
        let dir = tempfile::tempdir().unwrap();
        let created = create(CreateRunInput {
            workflow: WorkflowInput::DotSource {
                source: MINIMAL_DOT.to_string(),
                base_dir: None,
            },
            settings: FabroSettings {
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
            run_id: Some("run-123".to_string()),
            host_repo_path: Some(dir.path().display().to_string()),
            base_branch: Some("main".to_string()),
        })
        .unwrap();

        assert_eq!(created.run_id, "run-123");
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
        assert_eq!(
            crate::run_status::RunStatusRecord::load(&created.run_dir.join("status.json"))
                .unwrap()
                .status,
            crate::run_status::RunStatus::Submitted
        );
        assert!(!created.run_dir.join("id.txt").exists());
    }

    #[test]
    fn create_copies_workflow_toml_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_dir = dir.path().join("workflow");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(workflow_dir.join("workflow.fabro"), MINIMAL_DOT).unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "version = 1\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();

        let created = create(CreateRunInput {
            workflow: WorkflowInput::Path(workflow_dir.join("workflow.toml")),
            settings: FabroSettings {
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
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(created.run_dir.join("workflow.toml")).unwrap(),
            "version = 1\ngraph = \"workflow.fabro\"\n"
        );
    }

    #[test]
    fn create_resolves_working_directory_and_repo_path_from_request_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let created = create(CreateRunInput {
            workflow: WorkflowInput::DotSource {
                source: MINIMAL_DOT.to_string(),
                base_dir: None,
            },
            settings: FabroSettings {
                work_dir: Some("workspace".to_string()),
                dry_run: Some(true),
                ..Default::default()
            },
            cwd: dir.path().to_path_buf(),
            workflow_slug: None,
            run_dir: Some(dir.path().join("run")),
            run_id: Some("run-cwd".to_string()),
            host_repo_path: None,
            base_branch: None,
        })
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
}
