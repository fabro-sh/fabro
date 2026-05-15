#![expect(
    clippy::disallowed_methods,
    reason = "CLI manifest builder: sync file I/O building install manifests"
)]

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use fabro_api::types;
use fabro_config::project::{self, WorkflowLocation, discover_project_config};
use fabro_config::run::{resolve_run_goal_from_layer, resolve_run_goal_from_namespace};
use fabro_config::{
    CliLayer, DaytonaDockerfileLayer, DockerSandboxLayer, ReplaceMap, RunExecutionLayer,
    RunGoalLayer, RunLayer, RunModelLayer, RunSandboxLayer, WorkflowSettingsBuilder,
};
use fabro_graphviz::graph::AttrValue;
use fabro_graphviz::parser;
use fabro_template::{TemplateContext, render_lenient as render_scan_template};
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{ApprovalMode, ResolvedGoalSource, ResolvedRunGoal, RunMode};
use fabro_types::{DirtyStatus, GitContext, PreRunPushOutcome, RunId, WorkflowSettings};
use fabro_workflow::ManifestPath;
use fabro_workflow::git::{
    GitSyncStatus, branch_needs_push, head_sha, push_branch_noninteractive, sync_status,
};

#[derive(Debug, Default)]
pub struct ManifestBuildInput {
    pub workflow:           PathBuf,
    pub cwd:                PathBuf,
    pub run_overrides:      Option<RunLayer>,
    pub cli_overrides:      Option<CliLayer>,
    pub input_overrides:    HashMap<String, toml::Value>,
    pub args:               Option<types::ManifestArgs>,
    pub run_id:             Option<RunId>,
    /// Path to the user settings file (for inclusion in
    /// `RunManifest.configs`). `None` skips the user config entry.
    pub user_settings_path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct BuiltManifest {
    pub manifest:    types::RunManifest,
    pub target_path: PathBuf,
}

#[derive(Debug, Default)]
pub struct RunOverrideInput<'a> {
    pub goal:             Option<&'a str>,
    pub model:            Option<&'a str>,
    pub provider:         Option<&'a str>,
    pub sandbox:          Option<&'a str>,
    pub docker_image:     Option<&'a str>,
    pub preserve_sandbox: Option<bool>,
    pub dry_run:          Option<bool>,
    pub auto_approve:     Option<bool>,
    pub labels:           HashMap<String, String>,
}

#[must_use]
pub fn build_run_overrides(input: RunOverrideInput<'_>) -> RunLayer {
    let goal = input
        .goal
        .map(|goal| RunGoalLayer::Inline(InterpString::parse(goal)));
    let model = (input.model.is_some() || input.provider.is_some()).then(|| RunModelLayer {
        provider:  input.provider.map(InterpString::parse),
        name:      input.model.map(InterpString::parse),
        fallbacks: Vec::new(),
        controls:  None,
    });
    let sandbox = (input.sandbox.is_some()
        || input.docker_image.is_some()
        || input.preserve_sandbox.is_some())
    .then(|| RunSandboxLayer {
        provider: input.sandbox.map(ToOwned::to_owned),
        docker: input.docker_image.map(|image| DockerSandboxLayer {
            image: Some(image.to_string()),
            ..DockerSandboxLayer::default()
        }),
        preserve: input.preserve_sandbox,
        ..RunSandboxLayer::default()
    });
    let execution =
        (input.dry_run.is_some() || input.auto_approve.is_some()).then(|| RunExecutionLayer {
            mode:     input.dry_run.map(|dry_run| {
                if dry_run {
                    RunMode::DryRun
                } else {
                    RunMode::Normal
                }
            }),
            approval: input.auto_approve.map(|auto_approve| {
                if auto_approve {
                    ApprovalMode::Auto
                } else {
                    ApprovalMode::Prompt
                }
            }),
        });

    RunLayer {
        goal,
        metadata: ReplaceMap::from(input.labels),
        model,
        sandbox,
        execution,
        ..RunLayer::default()
    }
}

#[must_use]
pub fn build_sparse_run_overrides(input: RunOverrideInput<'_>) -> Option<RunLayer> {
    let run = build_run_overrides(input);
    (run.goal.is_some()
        || !run.metadata.is_empty()
        || run.model.is_some()
        || run.sandbox.is_some()
        || run.execution.is_some())
    .then_some(run)
}

struct CollectContext<'a> {
    cwd:               &'a Path,
    inputs:            &'a HashMap<String, toml::Value>,
    workflows:         HashMap<String, types::ManifestWorkflow>,
    visited_workflows: HashSet<String>,
}

#[derive(Clone)]
struct WorkflowScanInput {
    absolute_dot_path: PathBuf,
    dot_path:          ManifestPath,
    source:            String,
}

pub fn build_run_manifest(input: ManifestBuildInput) -> Result<BuiltManifest> {
    let root_location = WorkflowLocation::resolve(&input.workflow, &input.cwd)?;
    if root_location.toml.is_none() && !root_location.graph.is_file() {
        return Err(fabro_config::Error::WorkflowNotFound(
            root_location.graph.display().to_string(),
        )
        .into());
    }
    let project_config = discover_project_config(&root_location.dir)?;
    let project_config_source = project_config
        .as_ref()
        .map(|path| {
            let source = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let manifest_path = manifest_path_from_absolute(path, &input.cwd)?;
            Ok::<_, anyhow::Error>((path.clone(), manifest_path, source))
        })
        .transpose()?;

    let mut workflow_settings_builder = WorkflowSettingsBuilder::new();
    if let Some(run) = input.run_overrides.clone() {
        workflow_settings_builder = workflow_settings_builder.run_overrides(run);
    }
    if let Some(cli) = input.cli_overrides.clone() {
        workflow_settings_builder = workflow_settings_builder.cli_overrides(cli);
    }
    if let Some(path) = root_location.toml.as_ref() {
        workflow_settings_builder = workflow_settings_builder.workflow_file(path)?;
    }
    if let Some(path) = project_config.as_ref() {
        workflow_settings_builder = workflow_settings_builder.project_file(path)?;
    }
    if let Some(path) = input
        .user_settings_path
        .as_ref()
        .filter(|path| path.is_file())
    {
        workflow_settings_builder = workflow_settings_builder.user_file(path)?;
    }
    let mut workflow_settings = workflow_settings_builder
        .build()
        .context("failed to resolve manifest settings")?;
    workflow_settings.run.inputs.extend(input.input_overrides);
    let target_path = root_location.graph.clone();
    let target_manifest_path = manifest_path_from_absolute(&target_path, &input.cwd)?;
    let target_key = target_manifest_path.to_string();

    let mut context = CollectContext {
        cwd:               &input.cwd,
        inputs:            &workflow_settings.run.inputs,
        workflows:         HashMap::new(),
        visited_workflows: HashSet::new(),
    };
    collect_workflow_entry(&mut context, &input.workflow, &input.cwd)?;
    if let Some((_, config_path, source)) = project_config_source.as_ref() {
        let workflow = context
            .workflows
            .get_mut(&target_key)
            .ok_or_else(|| anyhow!("root workflow missing from manifest bundle"))?;
        collect_config_dockerfile(context.cwd, config_path, source, &mut workflow.files)?;
    }

    let root_source = context
        .workflows
        .get(&target_key)
        .map(|workflow| workflow.source.clone())
        .ok_or_else(|| anyhow!("root workflow missing from manifest bundle"))?;

    let mut configs = Vec::new();
    if let Some((path, _, source)) = project_config_source {
        configs.push(types::ManifestConfig {
            path:   Some(path.display().to_string()),
            source: Some(source),
            type_:  types::ManifestConfigType::Project,
        });
    }
    if let Some(path) = input.user_settings_path.filter(|p| p.is_file()) {
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        configs.push(types::ManifestConfig {
            path:   Some(path.display().to_string()),
            source: Some(source),
            type_:  types::ManifestConfigType::User,
        });
    }

    let working_directory =
        project::resolve_working_directory_from_run(&workflow_settings.run, &input.cwd);

    let rendered_root_source =
        render_workflow_scan_source(&root_source, &target_path, &workflow_settings.run.inputs)?;

    let goal = resolve_manifest_goal(
        input.run_overrides.as_ref(),
        &workflow_settings,
        &rendered_root_source,
        &target_path,
        &working_directory,
    )?;

    let configured_repo_origin_url = configured_repo_origin_url(&workflow_settings);
    let git = build_git_context(&working_directory, configured_repo_origin_url.as_deref());
    let args = input.args.filter(|args| !manifest_args_is_empty(args));

    Ok(BuiltManifest {
        manifest: types::RunManifest {
            args,
            configs,
            cwd: input.cwd.display().to_string(),
            git,
            goal,
            parent_id: None,
            run_id: input.run_id.map(|run_id| run_id.to_string()),
            title: None,
            target: types::ManifestTarget {
                identifier: input.workflow.display().to_string(),
                path:       target_key,
            },
            version: 1,
            workflows: context.workflows,
        },
        target_path,
    })
}

fn collect_workflow_entry(
    context: &mut CollectContext<'_>,
    workflow: &Path,
    resolve_from: &Path,
) -> Result<()> {
    let normalized_workflow = if workflow.extension().is_some() && workflow.is_relative() {
        normalize_absolute_path(resolve_from, &workflow.to_string_lossy()).ok_or_else(|| {
            anyhow!(
                "unsupported manifest workflow reference: {}",
                workflow.display()
            )
        })?
    } else {
        workflow.to_path_buf()
    };
    let location = WorkflowLocation::resolve(&normalized_workflow, resolve_from)?;
    let dot_path = manifest_path_from_absolute(&location.graph, context.cwd)?;
    let dot_key = dot_path.to_string();
    if !context.visited_workflows.insert(dot_key.clone()) {
        return Ok(());
    }

    let source = std::fs::read_to_string(&location.graph)
        .with_context(|| format!("Failed to read {}", location.graph.display()))?;
    let config = if let Some(workflow_toml_path) = location.toml.as_ref() {
        Some(types::ManifestWorkflowConfig {
            path:   manifest_path_from_absolute(workflow_toml_path, context.cwd)?.to_string(),
            source: std::fs::read_to_string(workflow_toml_path)
                .with_context(|| format!("Failed to read {}", workflow_toml_path.display()))?,
        })
    } else {
        None
    };

    let scan = WorkflowScanInput {
        absolute_dot_path: location.graph,
        dot_path,
        source: source.clone(),
    };
    let mut files = HashMap::new();
    let mut visited_imports = HashSet::new();
    if let Some(config) = config.as_ref() {
        let config_path = ManifestPath::from_wire(&config.path)
            .ok_or_else(|| anyhow!("invalid manifest workflow config path: {}", config.path))?;
        collect_config_dockerfile(context.cwd, &config_path, &config.source, &mut files)?;
    }
    collect_workflow_files(context, &scan, &mut files, &mut visited_imports)?;

    context.workflows.insert(dot_key, types::ManifestWorkflow {
        config,
        files,
        source,
    });

    Ok(())
}

fn collect_workflow_files(
    context: &mut CollectContext<'_>,
    workflow: &WorkflowScanInput,
    files: &mut HashMap<String, types::ManifestFileEntry>,
    visited_imports: &mut HashSet<String>,
) -> Result<()> {
    let rendered_source = render_workflow_scan_source(
        &workflow.source,
        &workflow.absolute_dot_path,
        context.inputs,
    )?;
    let graph = parser::parse(&rendered_source).map_err(|err| {
        anyhow!(
            "Failed to parse {}: {err}",
            workflow.absolute_dot_path.display()
        )
    })?;

    if let Some(goal_ref) = graph.attrs.get("goal").and_then(AttrValue::as_str) {
        if goal_ref.starts_with('@') {
            collect_bundled_file(
                files,
                workflow
                    .absolute_dot_path
                    .parent()
                    .unwrap_or_else(|| Path::new(".")),
                context.cwd,
                goal_ref.trim_start_matches('@'),
                types::ManifestFileRefType::FileInline,
                Some(workflow.dot_path.clone()),
            )?;
        }
    }

    for node in graph.nodes.values() {
        if let Some(prompt_ref) = node.attrs.get("prompt").and_then(AttrValue::as_str) {
            if prompt_ref.starts_with('@') {
                collect_bundled_file(
                    files,
                    workflow
                        .absolute_dot_path
                        .parent()
                        .unwrap_or_else(|| Path::new(".")),
                    context.cwd,
                    prompt_ref.trim_start_matches('@'),
                    types::ManifestFileRefType::FileInline,
                    Some(workflow.dot_path.clone()),
                )?;
            }
        }

        if let Some(import_ref) = node.attrs.get("import").and_then(AttrValue::as_str) {
            let imported = collect_bundled_file(
                files,
                workflow
                    .absolute_dot_path
                    .parent()
                    .unwrap_or_else(|| Path::new(".")),
                context.cwd,
                import_ref,
                types::ManifestFileRefType::Import,
                Some(workflow.dot_path.clone()),
            )?;
            let import_key = imported.path.to_string();
            if visited_imports.insert(import_key) {
                let imported_source = std::fs::read_to_string(&imported.absolute_path)
                    .with_context(|| {
                        format!("Failed to read {}", imported.absolute_path.display())
                    })?;
                let imported_scan = WorkflowScanInput {
                    absolute_dot_path: imported.absolute_path,
                    dot_path:          imported.path,
                    source:            imported_source,
                };
                collect_workflow_files(context, &imported_scan, files, visited_imports)?;
            }
        }

        if let Some(child_ref) = node
            .attrs
            .get("stack.child_workflow")
            .or_else(|| node.attrs.get("stack.child_dotfile"))
            .and_then(AttrValue::as_str)
        {
            collect_workflow_entry(
                context,
                Path::new(child_ref),
                workflow
                    .absolute_dot_path
                    .parent()
                    .unwrap_or_else(|| Path::new(".")),
            )?;
        }
    }

    Ok(())
}

fn render_workflow_scan_source(
    source: &str,
    path: &Path,
    inputs: &HashMap<String, toml::Value>,
) -> Result<String> {
    render_scan_template(source, &TemplateContext::for_input_scan(inputs.clone()))
        .with_context(|| format!("Failed to render {} for manifest scanning", path.display()))
}

fn collect_config_dockerfile(
    cwd: &Path,
    config_path: &ManifestPath,
    source: &str,
    files: &mut HashMap<String, types::ManifestFileEntry>,
) -> Result<()> {
    let mut document: toml::Table = source.parse().context("Failed to parse run config TOML")?;
    let run = document
        .remove("run")
        .map(toml::Value::try_into::<RunLayer>)
        .transpose()
        .context("Failed to parse run config TOML")?
        .unwrap_or_default();
    let dockerfile = run
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.daytona.as_ref())
        .and_then(|daytona| daytona.snapshot.as_ref())
        .and_then(|snapshot| snapshot.dockerfile.as_ref());

    let Some(DaytonaDockerfileLayer::Path { path }) = dockerfile else {
        return Ok(());
    };

    let absolute_config_path = cwd.join(config_path.as_path());
    collect_bundled_file(
        files,
        absolute_config_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
        cwd,
        path,
        types::ManifestFileRefType::Dockerfile,
        Some(config_path.clone()),
    )?;
    Ok(())
}

struct BundledFile {
    absolute_path: PathBuf,
    path:          ManifestPath,
}

fn collect_bundled_file(
    files: &mut HashMap<String, types::ManifestFileEntry>,
    base_dir: &Path,
    cwd: &Path,
    reference: &str,
    ref_type: types::ManifestFileRefType,
    from: Option<ManifestPath>,
) -> Result<BundledFile> {
    let absolute_path = normalize_absolute_path(base_dir, reference)
        .ok_or_else(|| anyhow!("unsupported manifest reference: {reference}"))?;
    let path = manifest_path_from_absolute(&absolute_path, cwd)?;
    let key = path.to_string();
    if !files.contains_key(&key) {
        let content = std::fs::read_to_string(&absolute_path)
            .with_context(|| format!("Failed to read {}", absolute_path.display()))?;
        files.insert(key.clone(), types::ManifestFileEntry {
            content,
            ref_: types::ManifestFileRef {
                from:     from.map(|value| value.to_string()),
                original: reference.to_string(),
                type_:    ref_type,
            },
        });
    }

    Ok(BundledFile {
        absolute_path,
        path,
    })
}

fn resolve_manifest_goal(
    run_overrides: Option<&RunLayer>,
    settings: &WorkflowSettings,
    root_source: &str,
    root_dot_path: &Path,
    working_directory: &Path,
) -> Result<Option<types::ManifestGoal>> {
    // Precedence 1: CLI args (`--goal` / `--goal-file`). These are already
    // resolved to absolute paths by `overrides::goal_layer_from_args`.
    if let Some(run_overrides) = run_overrides {
        if let Some(resolved) = resolve_run_goal_from_layer(run_overrides, working_directory)
            .context("failed to resolve --goal-file contents")?
        {
            return Ok(Some(resolved_goal_to_manifest(resolved)));
        }
    }

    // Precedence 2: merged config `run.goal`. Config-sourced `goal.file`
    // paths were rewritten to absolute by `load_settings_path` at the
    // directory of the config file that declared them.
    if let Some(resolved) = resolve_run_goal_from_namespace(&settings.run, working_directory)
        .context("failed to resolve run.goal.file contents")?
    {
        return Ok(Some(resolved_goal_to_manifest(resolved)));
    }

    // Precedence 3: graph-level `goal` attribute in the DOT, with `@file`
    // sugar for workflow-colocated goal files.
    let graph = parser::parse(root_source)
        .with_context(|| format!("Failed to parse {}", root_dot_path.display()))?;
    let Some(goal) = graph.attrs.get("goal").and_then(AttrValue::as_str) else {
        return Ok(None);
    };
    if let Some(reference) = goal.strip_prefix('@') {
        let goal_path = normalize_absolute_path(
            root_dot_path.parent().unwrap_or_else(|| Path::new(".")),
            reference,
        )
        .ok_or_else(|| anyhow!("unsupported manifest goal reference: {reference}"))?;
        return Ok(Some(types::ManifestGoal {
            path:  Some(reference.to_string()),
            text:  std::fs::read_to_string(&goal_path)
                .with_context(|| format!("Failed to read {}", goal_path.display()))?,
            type_: types::ManifestGoalType::Graph,
        }));
    }

    Ok(Some(types::ManifestGoal {
        path:  None,
        text:  goal.to_string(),
        type_: types::ManifestGoalType::Graph,
    }))
}

/// Translate a [`ResolvedRunGoal`] into the wire-level `ManifestGoal`
/// shape. Inline goals get `type = Value`; file-sourced goals keep their
/// absolute path as the `path` field and use `type = File`.
fn resolved_goal_to_manifest(resolved: ResolvedRunGoal) -> types::ManifestGoal {
    match resolved.source {
        ResolvedGoalSource::Inline => types::ManifestGoal {
            path:  None,
            text:  resolved.text,
            type_: types::ManifestGoalType::Value,
        },
        ResolvedGoalSource::File { path } => types::ManifestGoal {
            path:  Some(path.to_string_lossy().into_owned()),
            text:  resolved.text,
            type_: types::ManifestGoalType::File,
        },
    }
}

fn build_git_context(
    repo_path: &Path,
    configured_repo_origin_url: Option<&str>,
) -> Option<GitContext> {
    let (origin_url, branch) = detect_manifest_repo_info(repo_path)?;
    let sha = head_sha(repo_path).ok();
    let dirty = match sync_status(repo_path, "origin", Some(&branch)) {
        GitSyncStatus::Dirty => DirtyStatus::Dirty,
        GitSyncStatus::Synced | GitSyncStatus::Unsynced => DirtyStatus::Clean,
    };
    let repo_origin_url = configured_repo_origin_url
        .map(fabro_github::normalize_repo_origin_url)
        .filter(|url| !url.is_empty())
        .or_else(|| {
            origin_url
                .as_deref()
                .map(fabro_github::normalize_repo_origin_url)
                .filter(|url| !url.is_empty())
        })
        .unwrap_or_default();
    let push_outcome = build_manifest_push_outcome(
        repo_path,
        &branch,
        origin_url.as_deref(),
        configured_repo_origin_url,
    );
    Some(GitContext {
        origin_url: repo_origin_url,
        branch,
        sha,
        dirty,
        push_outcome,
    })
}

fn configured_repo_origin_url(settings: &WorkflowSettings) -> Option<String> {
    let scm = &settings.run.scm;
    if !scm
        .provider
        .as_deref()
        .is_none_or(|provider| provider.eq_ignore_ascii_case("github"))
    {
        return None;
    }
    let owner = scm.owner.as_ref()?.as_source();
    let repository = scm.repository.as_ref()?.as_source();
    if owner.trim().is_empty() || repository.trim().is_empty() {
        return None;
    }
    let origin = format!("https://github.com/{owner}/{repository}");
    let normalized = fabro_github::normalize_repo_origin_url(&origin);
    (!normalized.is_empty()).then_some(normalized)
}

fn detect_manifest_repo_info(repo_path: &Path) -> Option<(Option<String>, String)> {
    let repo = git2::Repository::discover(repo_path).ok()?;
    let branch = repo.head().ok()?.shorthand().map(ToOwned::to_owned)?;
    let origin_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|remote| remote.url().map(ToOwned::to_owned));
    Some((origin_url, branch))
}

fn build_manifest_push_outcome(
    repo_path: &Path,
    branch: &str,
    origin_url: Option<&str>,
    configured_repo_origin_url: Option<&str>,
) -> PreRunPushOutcome {
    let Some(origin_url) = origin_url else {
        return PreRunPushOutcome::SkippedNoRemote;
    };

    if let Some(repo_origin_url) = configured_repo_origin_url
        .map(fabro_github::normalize_repo_origin_url)
        .filter(|url| !url.is_empty())
    {
        let remote = fabro_github::normalize_repo_origin_url(origin_url);
        if remote != repo_origin_url {
            return PreRunPushOutcome::SkippedRemoteMismatch {
                remote,
                repo_origin_url,
            };
        }
    }

    if !branch_needs_push(repo_path, "origin", branch) {
        return PreRunPushOutcome::NotAttempted;
    }

    match push_branch_noninteractive(repo_path, "origin", branch) {
        Ok(()) => PreRunPushOutcome::Succeeded {
            remote: "origin".to_string(),
            branch: branch.to_string(),
        },
        Err(err) => PreRunPushOutcome::Failed {
            remote:  "origin".to_string(),
            branch:  branch.to_string(),
            message: err.to_string(),
        },
    }
}

fn normalize_absolute_path(base_dir: &Path, reference: &str) -> Option<PathBuf> {
    let path = Path::new(reference);
    if path.is_absolute() || reference.starts_with('~') {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in base_dir.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir => normalized.push(Path::new("/")),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }
    Some(normalized)
}

fn manifest_path_from_absolute(path: &Path, cwd: &Path) -> Result<ManifestPath> {
    ManifestPath::from_absolute(path, cwd)
        .ok_or_else(|| anyhow!("Failed to compute manifest path for {}", path.display()))
}

pub fn manifest_args_is_empty(args: &types::ManifestArgs) -> bool {
    args.auto_approve.is_none()
        && args.dry_run.is_none()
        && args.label.is_empty()
        && args.model.is_none()
        && args.preserve_sandbox.is_none()
        && args.provider.is_none()
        && args.sandbox.is_none()
        && args.docker_image.is_none()
        && args.input.is_empty()
        && args.verbose.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_run_overrides_sets_common_cli_and_mcp_layers() {
        let overrides = build_run_overrides(RunOverrideInput {
            goal:             Some("ship it"),
            model:            Some("gpt-5.4-mini"),
            provider:         Some("openai"),
            sandbox:          Some("local"),
            docker_image:     None,
            preserve_sandbox: Some(true),
            dry_run:          Some(true),
            auto_approve:     Some(false),
            labels:           [("source".to_string(), "mcp".to_string())]
                .into_iter()
                .collect(),
        });

        let goal = overrides.goal.expect("goal override");
        assert!(matches!(goal, fabro_config::RunGoalLayer::Inline(_)));
        assert_eq!(
            overrides
                .model
                .as_ref()
                .unwrap()
                .name
                .as_ref()
                .unwrap()
                .as_source(),
            "gpt-5.4-mini"
        );
        assert_eq!(
            overrides
                .model
                .as_ref()
                .unwrap()
                .provider
                .as_ref()
                .unwrap()
                .as_source(),
            "openai"
        );
        assert_eq!(
            overrides.sandbox.as_ref().unwrap().provider.as_deref(),
            Some("local")
        );
        assert_eq!(overrides.sandbox.as_ref().unwrap().preserve, Some(true));
        assert_eq!(
            overrides.execution.as_ref().unwrap().mode,
            Some(RunMode::DryRun)
        );
        assert_eq!(
            overrides.execution.as_ref().unwrap().approval,
            Some(ApprovalMode::Prompt)
        );
        assert_eq!(
            overrides.metadata.0.get("source").map(String::as_str),
            Some("mcp")
        );
    }

    #[test]
    fn build_manifest_bundles_imports_prompts_and_children() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        let child_dir = project.join(".fabro/workflows/child");
        std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();
        std::fs::create_dir_all(workflow_dir.join("imports")).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(project.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r#"digraph Demo {
                graph [goal="@prompts/goal.md"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                plan [prompt="@prompts/plan.md"]
                imported [import="./imports/checks.fabro"]
                child [shape=house, stack.child_workflow="../child/workflow.fabro"]
                start -> plan -> imported -> child -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/goal.md"), "ship it").unwrap();
        std::fs::write(workflow_dir.join("prompts/plan.md"), "plan it").unwrap();
        std::fs::write(
            workflow_dir.join("imports/checks.fabro"),
            r#"digraph Checks {
                start [shape=Mdiamond]
                exit [shape=Msquare]
                lint [prompt="@../prompts/lint.md"]
                start -> lint -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/lint.md"), "lint it").unwrap();
        std::fs::write(
            child_dir.join("workflow.fabro"),
            r"digraph Child { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(
            built.manifest.target.path,
            ".fabro/workflows/demo/workflow.fabro"
        );
        assert_eq!(built.manifest.workflows.len(), 2);
        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/prompts/goal.md")
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/prompts/plan.md")
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/imports/checks.fabro")
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/prompts/lint.md")
        );
        assert_eq!(built.manifest.goal.unwrap().text, "ship it");
        assert!(
            built
                .manifest
                .workflows
                .contains_key(".fabro/workflows/child/workflow.fabro")
        );
    }

    #[test]
    fn build_manifest_bundles_project_config_daytona_dockerfile_relative_to_project_config() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();

        std::fs::write(
            project.join(".fabro/project.toml"),
            r#"_version = 1

[run.sandbox.daytona.snapshot]
name = "fabro-test"
dockerfile = { path = "Dockerfile" }
"#,
        )
        .unwrap();
        std::fs::write(project.join(".fabro/Dockerfile"), "FROM ubuntu:24.04\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        let entry = root
            .files
            .get(".fabro/Dockerfile")
            .expect("project Dockerfile should be bundled with target workflow");
        assert_eq!(entry.content, "FROM ubuntu:24.04\n");
        assert_eq!(entry.ref_.type_, types::ManifestFileRefType::Dockerfile);
        assert_eq!(entry.ref_.original, "Dockerfile");
        assert_eq!(entry.ref_.from.as_deref(), Some(".fabro/project.toml"));
    }

    #[test]
    fn build_manifest_uses_input_overrides_for_structural_file_scanning() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        let child_dir = project.join(".fabro/workflows/child");
        std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();
        std::fs::create_dir_all(workflow_dir.join("imports")).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(project.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r#"digraph Demo {
                graph [goal="Demo"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                plan [prompt="@prompts/{{ inputs.prompt_file }}"]
                imported [import="./imports/{{ inputs.import_file }}"]
                child [shape=house, stack.child_workflow="../{{ inputs.child_workflow }}/workflow.fabro"]
                start -> plan -> imported -> child -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/plan.md"), "plan it").unwrap();
        std::fs::write(
            workflow_dir.join("imports/checks.fabro"),
            r"digraph Checks { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();
        std::fs::write(
            child_dir.join("workflow.fabro"),
            r"digraph Child { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            input_overrides: HashMap::from([
                (
                    "prompt_file".to_string(),
                    toml::Value::String("plan.md".to_string()),
                ),
                (
                    "import_file".to_string(),
                    toml::Value::String("checks.fabro".to_string()),
                ),
                (
                    "child_workflow".to_string(),
                    toml::Value::String("child".to_string()),
                ),
            ]),
            ..Default::default()
        })
        .unwrap();

        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        assert!(
            root.source.contains("{{ inputs.prompt_file }}"),
            "manifest should store original workflow source"
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/prompts/plan.md")
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/imports/checks.fabro")
        );
        assert!(
            built
                .manifest
                .workflows
                .contains_key(".fabro/workflows/child/workflow.fabro")
        );
    }

    #[test]
    fn build_manifest_uses_input_overrides_for_graph_goal_file_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();
        std::fs::write(project.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r#"digraph Demo {
                graph [goal="@prompts/{{ inputs.goal_file }}"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/goal.md"), "ship it").unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            input_overrides: HashMap::from([(
                "goal_file".to_string(),
                toml::Value::String("goal.md".to_string()),
            )]),
            ..Default::default()
        })
        .unwrap();

        let goal = built.manifest.goal.expect("manifest goal should resolve");
        assert_eq!(goal.path.as_deref(), Some("prompts/goal.md"));
        assert_eq!(goal.text, "ship it");
        assert_eq!(goal.type_, types::ManifestGoalType::Graph);
        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        assert!(
            root.source.contains("{{ inputs.goal_file }}"),
            "manifest should store original workflow source"
        );
    }

    /// A relative `[run.goal] file = "..."` declared in `.fabro/project.toml`
    /// must resolve against the directory of `.fabro/project.toml`, not against
    /// the invocation cwd. We exercise this by invoking from a subdirectory
    /// below the project root.
    #[test]
    fn build_manifest_resolves_relative_goal_file_in_project_config() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::create_dir_all(project.join(".fabro/prompts")).unwrap();

        std::fs::write(
            project.join(".fabro/project.toml"),
            r#"_version = 1

[run.goal]
file = "prompts/goal.md"
"#,
        )
        .unwrap();
        std::fs::write(
            project.join(".fabro/prompts/goal.md"),
            "ship from project root",
        )
        .unwrap();

        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let goal = built.manifest.goal.expect("manifest goal should be set");
        assert_eq!(goal.text, "ship from project root");
        assert_eq!(goal.type_, types::ManifestGoalType::File);
        let resolved = goal.path.expect("file goal must carry a path");
        let expected = project.join(".fabro").join("prompts").join("goal.md");
        assert_eq!(PathBuf::from(resolved), expected);
    }

    /// A relative `[run.goal] file = "..."` declared in `workflow.toml`
    /// must resolve against the directory of `workflow.toml`, not against
    /// the invocation cwd or project root.
    #[test]
    fn build_manifest_resolves_relative_goal_file_in_workflow_config() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();

        std::fs::write(project.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            r#"_version = 1

[workflow]
graph = "workflow.fabro"

[run.goal]
file = "prompts/goal.md"
"#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("prompts/goal.md"),
            "ship from workflow dir",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let goal = built.manifest.goal.expect("manifest goal should be set");
        assert_eq!(goal.text, "ship from workflow dir");
        assert_eq!(goal.type_, types::ManifestGoalType::File);
        let resolved = goal.path.expect("file goal must carry a path");
        let expected = workflow_dir.join("prompts").join("goal.md");
        assert_eq!(PathBuf::from(resolved), expected);
    }

    /// When `[run] working_dir` points to a nested git repo, the manifest's
    /// `git.branch` and `git.origin_url` must come from that target repo, not
    /// from an enclosing workspace repo that happens to be the CLI's cwd.
    /// Regression test for https://github.com/fabro-sh/fabro/issues/159.
    #[test]
    fn build_manifest_git_follows_working_directory_into_nested_repo() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();
        let target = workspace.join("repos").join("target");
        std::fs::create_dir_all(&target).unwrap();

        init_git_repo(
            workspace,
            "workspace-branch",
            "https://github.com/example/workspace.git",
        );
        mark_origin_branch_synced(workspace, "workspace-branch");
        init_git_repo(
            &target,
            "target-branch",
            "https://github.com/example/target.git",
        );
        mark_origin_branch_synced(&target, "target-branch");

        let workflow_dir = workspace.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(
            workspace.join(".fabro/project.toml"),
            r#"_version = 1

[run]
working_dir = "repos/target"
"#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: workspace.to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let git = built
            .manifest
            .git
            .expect("manifest git info should be detected");
        assert_eq!(git.branch, "target-branch");
        assert_eq!(git.origin_url, "https://github.com/example/target");
        assert_eq!(git.push_outcome, PreRunPushOutcome::NotAttempted);
    }

    #[test]
    fn build_manifest_git_skips_push_when_configured_repository_differs_from_origin() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path();

        init_git_repo(
            workspace,
            "feature",
            "https://github.com/user/forked-target.git",
        );

        let workflow_dir = workspace.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(
            workspace.join(".fabro/project.toml"),
            r#"_version = 1

[run.scm]
provider = "github"
owner = "example"
repository = "target"
"#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: workspace.to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let git = built
            .manifest
            .git
            .expect("manifest git info should be detected");
        assert_eq!(git.origin_url, "https://github.com/example/target");
        assert_eq!(git.push_outcome, PreRunPushOutcome::SkippedRemoteMismatch {
            remote:          "https://github.com/user/forked-target".to_string(),
            repo_origin_url: "https://github.com/example/target".to_string(),
        });
    }

    #[cfg(unix)]
    #[test]
    fn build_manifest_push_attempt_disables_terminal_prompts() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        init_git_repo(&workspace, "feature", "fabro-prompt-test::target");

        let helper_dir = temp.path().join("bin");
        std::fs::create_dir_all(&helper_dir).unwrap();
        let helper_path = helper_dir.join("git-remote-fabro-prompt-test");
        std::fs::write(
            &helper_path,
            r#"#!/bin/sh
printf '%s\n' "${GIT_TERMINAL_PROMPT-unset}" > "$FABRO_PROMPT_ENV_LOG"
echo "helper saw GIT_TERMINAL_PROMPT=${GIT_TERMINAL_PROMPT-unset}" >&2
exit 1
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper_path, permissions).unwrap();

        let workflow_dir = workspace.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(workspace.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .unwrap();

        let helper_log = temp.path().join("prompt-env.txt");
        let mut path_entries = vec![helper_dir];
        if let Some(path) = std::env::var_os("PATH") {
            path_entries.extend(std::env::split_paths(&path));
        }
        let path = std::env::join_paths(path_entries).unwrap();
        temp_env::with_var("PATH", Some(path), || {
            temp_env::with_var("FABRO_PROMPT_ENV_LOG", Some(helper_log.as_os_str()), || {
                let built = build_run_manifest(ManifestBuildInput {
                    workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
                    cwd: workspace.clone(),
                    ..Default::default()
                })
                .unwrap();

                let git = built
                    .manifest
                    .git
                    .expect("manifest git info should be detected");
                assert!(matches!(git.push_outcome, PreRunPushOutcome::Failed { .. }));
            });
        });

        assert_eq!(std::fs::read_to_string(helper_log).unwrap(), "0\n");
    }

    fn init_git_repo(path: &Path, branch: &str, origin_url: &str) {
        run_git(path, &[
            "-c",
            &format!("init.defaultBranch={branch}"),
            "init",
            "--quiet",
        ]);
        run_git(path, &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "init",
        ]);
        run_git(path, &["remote", "add", "origin", origin_url]);
    }

    fn mark_origin_branch_synced(path: &Path, branch: &str) {
        let remote_ref = format!("refs/remotes/origin/{branch}");
        run_git(path, &["update-ref", &remote_ref, "HEAD"]);
    }

    fn run_git(path: &Path, args: &[&str]) {
        use std::process::Command;
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
