use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{Local, Utc};
use fabro_config::FabroSettings;
use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_model::{Catalog, Provider};

use crate::error::FabroError;
use crate::pipeline::types::PersistOptions;
use crate::pipeline::{self, Persisted, TransformOptions, Validated};
use crate::records::RunRecord;
use crate::transforms::{expand_vars, Transform};

#[derive(Default)]
pub struct ValidateOptions {
    pub base_dir: Option<PathBuf>,
    pub custom_transforms: Vec<Box<dyn Transform>>,
    pub settings: Option<FabroSettings>,
    pub goal_override: Option<String>,
}

pub struct RunCreateOptions {
    pub settings: FabroSettings,
    pub run_dir: Option<PathBuf>,
    pub run_id: Option<String>,
    pub workflow_slug: Option<String>,
    pub labels: HashMap<String, String>,
    pub base_branch: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub host_repo_path: Option<String>,
    pub goal_override: Option<String>,
    pub base_dir: Option<PathBuf>,
}

/// Parse, transform, and validate a DOT source string.
///
/// Returns `Validated` even when validation produced errors. Call
/// `validated.raise_on_errors()` if the caller wants to fail fast.
pub fn validate(dot_source: &str, options: ValidateOptions) -> Result<Validated, FabroError> {
    preprocess_and_validate(
        dot_source,
        options.base_dir,
        options.custom_transforms,
        options.settings.as_ref(),
        options.goal_override.as_deref(),
    )
}

/// Read a DOT file, apply file inlining from its parent directory, then validate.
pub fn validate_from_file(path: &Path) -> Result<Validated, FabroError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| FabroError::Parse(format!("Failed to read {}: {e}", path.display())))?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    validate(
        &source,
        ValidateOptions {
            base_dir: Some(base_dir.to_path_buf()),
            ..Default::default()
        },
    )
}

/// Parse, transform, validate, resolve settings, and persist a run.
pub fn create(dot_source: &str, options: RunCreateOptions) -> Result<Persisted, FabroError> {
    let validated = preprocess_and_validate(
        dot_source,
        options.base_dir.clone(),
        Vec::new(),
        Some(&options.settings),
        options.goal_override.as_deref(),
    )?;

    if validated.has_errors() {
        return Err(FabroError::ValidationFailed {
            diagnostics: validated.diagnostics().to_vec(),
        });
    }

    persist_validated(validated, options)
}

/// Read a DOT file, apply file inlining from its parent directory, then create.
pub fn create_from_file(
    path: &Path,
    mut options: RunCreateOptions,
) -> Result<Persisted, FabroError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| FabroError::Parse(format!("Failed to read {}: {e}", path.display())))?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    options.base_dir = Some(base_dir.to_path_buf());
    create(&source, options)
}

fn preprocess_and_validate(
    dot_source: &str,
    base_dir: Option<PathBuf>,
    custom_transforms: Vec<Box<dyn Transform>>,
    settings: Option<&FabroSettings>,
    goal_override: Option<&str>,
) -> Result<Validated, FabroError> {
    let source = match settings.and_then(|resolved| resolved.vars.as_ref()) {
        Some(vars) => {
            let mut vars = vars.clone();
            // `$goal` is resolved later from the graph goal after any goal override.
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
    options: RunCreateOptions,
) -> Result<Persisted, FabroError> {
    let RunCreateOptions {
        settings,
        run_dir,
        run_id,
        workflow_slug,
        labels,
        base_branch,
        working_directory,
        host_repo_path,
        goal_override: _,
        base_dir: _,
    } = options;

    let settings = resolve_run_settings(settings, validated.graph());

    let run_id = run_id.unwrap_or_else(|| ulid::Ulid::new().to_string());
    let run_dir = run_dir.unwrap_or_else(|| default_run_dir(&run_id, settings.dry_run_enabled()));
    let working_directory = working_directory
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

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

    let model = configured_model
        .or(graph_model)
        .map(str::to_string)
        .unwrap_or_else(|| {
            let catalog = Catalog::builtin();
            provider
                .as_deref()
                .and_then(|value| value.parse::<Provider>().ok())
                .and_then(|provider| catalog.default_for_provider(provider))
                .unwrap_or_else(|| catalog.default_from_env())
                .id
                .clone()
        });

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

pub fn default_run_dir(run_id: &str, dry_run: bool) -> PathBuf {
    make_run_dir(&crate::run_lookup::default_runs_base(), run_id, dry_run)
}

pub fn make_run_dir(runs_base: &Path, run_id: &str, dry_run: bool) -> PathBuf {
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

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    #[test]
    fn validate_minimal() {
        let validated = validate(MINIMAL_DOT, ValidateOptions::default()).unwrap();
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
        let validated = validate(dot, ValidateOptions::default()).unwrap();
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
        let validated = validate(dot, ValidateOptions::default()).unwrap();
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
        let validated = validate(
            dot,
            ValidateOptions {
                settings: Some(FabroSettings {
                    vars: Some(HashMap::from([("who".to_string(), "agent".to_string())])),
                    ..Default::default()
                }),
                goal_override: Some("override".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
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
        let result = validate("not a graph", ValidateOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn validate_returns_validation_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let validated = validate(dot, ValidateOptions::default()).unwrap();

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

        let validated = validate(
            MINIMAL_DOT,
            ValidateOptions {
                custom_transforms: vec![Box::new(TagTransform)],
                ..Default::default()
            },
        )
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

        let validated = validate_from_file(&dot_path).unwrap();
        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "ship it");
    }

    #[test]
    fn create_returns_validation_failed_with_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let err = create(
            dot,
            RunCreateOptions {
                settings: FabroSettings::default(),
                run_dir: None,
                run_id: None,
                workflow_slug: None,
                labels: HashMap::new(),
                base_branch: None,
                working_directory: None,
                host_repo_path: None,
                goal_override: None,
                base_dir: None,
            },
        )
        .unwrap_err();

        match err {
            FabroError::ValidationFailed { diagnostics } => {
                assert!(!diagnostics.is_empty());
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn create_persists_normalized_config() {
        let dir = tempfile::tempdir().unwrap();
        let persisted = create(
            MINIMAL_DOT,
            RunCreateOptions {
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
                    dry_run: Some(true),
                    ..Default::default()
                },
                run_dir: Some(dir.path().join("run")),
                run_id: Some("run-123".to_string()),
                workflow_slug: Some("slug".to_string()),
                labels: HashMap::from([("env".to_string(), "test".to_string())]),
                base_branch: Some("main".to_string()),
                working_directory: Some(dir.path().to_path_buf()),
                host_repo_path: Some(dir.path().display().to_string()),
                goal_override: Some("override goal".to_string()),
                base_dir: None,
            },
        )
        .unwrap();

        assert_eq!(persisted.run_record().run_id, "run-123");
        assert_eq!(persisted.run_record().graph.goal(), "override goal");
        assert_eq!(
            persisted
                .run_record()
                .settings
                .llm
                .as_ref()
                .and_then(|llm| llm.model.as_deref()),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            persisted
                .run_record()
                .settings
                .llm
                .as_ref()
                .and_then(|llm| llm.provider.as_deref()),
            Some("anthropic")
        );
        assert_eq!(
            persisted.run_record().settings.goal.as_deref(),
            Some("override goal")
        );
        assert!(persisted.run_record().settings.pull_request.is_none());
        assert_eq!(
            persisted.run_record().workflow_slug.as_deref(),
            Some("slug")
        );
    }
}
