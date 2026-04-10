use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use fabro_api::types;
use fabro_config::load::{load_settings_for_workflow, load_settings_user};
use fabro_config::merge::combine_files;
use fabro_config::project::{self, discover_project_config, resolve_workflow_path};
use fabro_config::run::{parse_run_config, resolve_run_goal};
use fabro_config::user::active_settings_path;
use fabro_graphviz::graph::AttrValue;
use fabro_graphviz::parser;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_types::RunId;
use fabro_types::settings::SettingsFile;
use fabro_types::settings::run::{DaytonaDockerfileLayer, ResolvedGoalSource, ResolvedRunGoal};
use fabro_workflow::git::{GitSyncStatus, head_sha, sync_status};

use crate::args::{PreflightArgs, RunArgs};

#[derive(Debug)]
pub(crate) struct ManifestBuildInput {
    pub workflow: PathBuf,
    pub cwd: PathBuf,
    pub args_layer: SettingsFile,
    pub args: Option<types::ManifestArgs>,
    pub run_id: Option<RunId>,
}

#[derive(Debug)]
pub(crate) struct BuiltManifest {
    pub manifest: types::RunManifest,
    pub target_path: PathBuf,
}

struct CollectContext<'a> {
    cwd: &'a Path,
    workflows: HashMap<String, types::ManifestWorkflow>,
    visited_workflows: HashSet<String>,
}

#[derive(Clone)]
struct WorkflowScanInput {
    absolute_dot_path: PathBuf,
    logical_dot_path: PathBuf,
    source: String,
}

pub(crate) fn build_run_manifest(input: ManifestBuildInput) -> Result<BuiltManifest> {
    let user_layer = load_settings_user()?;
    let workflow_layer = load_settings_for_workflow(&input.workflow, &input.cwd)?;
    let merged_settings = combine_files(
        combine_files(user_layer, workflow_layer),
        input.args_layer.clone(),
    );

    let root_resolution = resolve_workflow_path(&input.workflow, &input.cwd)?;
    let target_path = root_resolution.dot_path.clone();
    let target_logical_path = to_logical_path(&target_path, &input.cwd)?;
    let target_logical_path_string = logical_path_string(&target_logical_path);

    let mut context = CollectContext {
        cwd: &input.cwd,
        workflows: HashMap::new(),
        visited_workflows: HashSet::new(),
    };
    collect_workflow_entry(&mut context, &input.workflow, &input.cwd)?;

    let root_source = context
        .workflows
        .get(&target_logical_path_string)
        .map(|workflow| workflow.source.clone())
        .ok_or_else(|| anyhow!("root workflow missing from manifest bundle"))?;

    let mut configs = Vec::new();
    if let Some((path, _config)) = discover_project_config(
        root_resolution
            .resolved_workflow_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )? {
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        configs.push(types::ManifestConfig {
            path: Some(path.display().to_string()),
            source: Some(source),
            type_: types::ManifestConfigType::Project,
        });
    }
    let settings_path = active_settings_path(None);
    if settings_path.is_file() {
        let path = settings_path;
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        configs.push(types::ManifestConfig {
            path: Some(path.display().to_string()),
            source: Some(source),
            type_: types::ManifestConfigType::User,
        });
    }

    let goal = resolve_manifest_goal(
        &input.args_layer,
        &merged_settings,
        &root_source,
        &target_path,
        &input.cwd,
    )?;

    let git = build_manifest_git(&input.cwd);
    let args = input.args.filter(|args| !manifest_args_is_empty(args));

    Ok(BuiltManifest {
        manifest: types::RunManifest {
            args,
            configs,
            cwd: input.cwd.display().to_string(),
            git,
            goal,
            run_id: input.run_id.map(|run_id| run_id.to_string()),
            target: types::ManifestTarget {
                identifier: input.workflow.display().to_string(),
                path: target_logical_path_string,
            },
            version: 1,
            workflows: context.workflows,
        },
        target_path,
    })
}

pub(crate) fn run_manifest_args(args: &RunArgs) -> Option<types::ManifestArgs> {
    let payload = types::ManifestArgs {
        auto_approve: args.auto_approve.then_some(true),
        dry_run: args.dry_run.then_some(true),
        label: args.label.clone(),
        model: args.model.clone(),
        no_retro: args.no_retro.then_some(true),
        preserve_sandbox: args.preserve_sandbox.then_some(true),
        provider: args.provider.clone(),
        sandbox: args
            .sandbox
            .map(|provider| fabro_sandbox::SandboxProvider::from(provider).to_string()),
        verbose: args.verbose.then_some(true),
    };
    (!manifest_args_is_empty(&payload)).then_some(payload)
}

pub(crate) fn preflight_manifest_args(args: &PreflightArgs) -> Option<types::ManifestArgs> {
    let payload = types::ManifestArgs {
        auto_approve: None,
        dry_run: None,
        label: Vec::new(),
        model: args.model.clone(),
        no_retro: None,
        preserve_sandbox: None,
        provider: args.provider.clone(),
        sandbox: args
            .sandbox
            .map(|provider| fabro_sandbox::SandboxProvider::from(provider).to_string()),
        verbose: args.verbose.then_some(true),
    };
    (!manifest_args_is_empty(&payload)).then_some(payload)
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
    let resolution = resolve_workflow_path(&normalized_workflow, resolve_from)?;
    let logical_dot_path = to_logical_path(&resolution.dot_path, context.cwd)?;
    let logical_dot_key = logical_path_string(&logical_dot_path);
    if !context.visited_workflows.insert(logical_dot_key.clone()) {
        return Ok(());
    }

    let source = std::fs::read_to_string(&resolution.dot_path)
        .with_context(|| format!("Failed to read {}", resolution.dot_path.display()))?;
    let config = if let Some(workflow_toml_path) = resolution.workflow_toml_path.as_ref() {
        Some(types::ManifestWorkflowConfig {
            path: logical_path_string(&to_logical_path(workflow_toml_path, context.cwd)?),
            source: std::fs::read_to_string(workflow_toml_path)
                .with_context(|| format!("Failed to read {}", workflow_toml_path.display()))?,
        })
    } else {
        None
    };

    let scan = WorkflowScanInput {
        absolute_dot_path: resolution.dot_path,
        logical_dot_path,
        source: source.clone(),
    };
    let mut files = HashMap::new();
    let mut visited_imports = HashSet::new();
    if let Some(config) = config.as_ref() {
        collect_workflow_config_files(context, config, &mut files)?;
    }
    collect_workflow_files(context, &scan, &mut files, &mut visited_imports)?;

    context.workflows.insert(
        logical_dot_key,
        types::ManifestWorkflow {
            config,
            files,
            source,
        },
    );

    Ok(())
}

fn collect_workflow_files(
    context: &mut CollectContext<'_>,
    workflow: &WorkflowScanInput,
    files: &mut HashMap<String, types::ManifestFileEntry>,
    visited_imports: &mut HashSet<String>,
) -> Result<()> {
    let graph = parser::parse(&workflow.source).map_err(|err| {
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
                Some(workflow.logical_dot_path.clone()),
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
                    Some(workflow.logical_dot_path.clone()),
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
                Some(workflow.logical_dot_path.clone()),
            )?;
            let import_key = logical_path_string(&imported.logical_path);
            if visited_imports.insert(import_key) {
                let imported_source = std::fs::read_to_string(&imported.absolute_path)
                    .with_context(|| {
                        format!("Failed to read {}", imported.absolute_path.display())
                    })?;
                let imported_scan = WorkflowScanInput {
                    absolute_dot_path: imported.absolute_path,
                    logical_dot_path: imported.logical_path,
                    source: imported_source,
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

fn collect_workflow_config_files(
    context: &CollectContext<'_>,
    config: &types::ManifestWorkflowConfig,
    files: &mut HashMap<String, types::ManifestFileEntry>,
) -> Result<()> {
    let config_layer = parse_run_config(&config.source)?;
    let dockerfile = config_layer
        .run
        .as_ref()
        .and_then(|run| run.sandbox.as_ref())
        .and_then(|sandbox| sandbox.daytona.as_ref())
        .and_then(|daytona| daytona.snapshot.as_ref())
        .and_then(|snapshot| snapshot.dockerfile.as_ref());

    let Some(DaytonaDockerfileLayer::Path { path }) = dockerfile else {
        return Ok(());
    };

    let config_path = context.cwd.join(&config.path);
    collect_bundled_file(
        files,
        config_path.parent().unwrap_or_else(|| Path::new(".")),
        context.cwd,
        path,
        types::ManifestFileRefType::Dockerfile,
        Some(PathBuf::from(&config.path)),
    )?;
    Ok(())
}

struct BundledFile {
    absolute_path: PathBuf,
    logical_path: PathBuf,
}

fn collect_bundled_file(
    files: &mut HashMap<String, types::ManifestFileEntry>,
    base_dir: &Path,
    cwd: &Path,
    reference: &str,
    ref_type: types::ManifestFileRefType,
    from: Option<PathBuf>,
) -> Result<BundledFile> {
    let absolute_path = normalize_absolute_path(base_dir, reference)
        .ok_or_else(|| anyhow!("unsupported manifest reference: {reference}"))?;
    let logical_path = to_logical_path(&absolute_path, cwd)?;
    let key = logical_path_string(&logical_path);
    if !files.contains_key(&key) {
        let content = std::fs::read_to_string(&absolute_path)
            .with_context(|| format!("Failed to read {}", absolute_path.display()))?;
        files.insert(
            key.clone(),
            types::ManifestFileEntry {
                content,
                ref_: types::ManifestFileRef {
                    from: from.map(|value| logical_path_string(&value)),
                    original: reference.to_string(),
                    type_: ref_type,
                },
            },
        );
    }

    Ok(BundledFile {
        absolute_path,
        logical_path,
    })
}

fn resolve_manifest_goal(
    args_layer: &SettingsFile,
    settings: &SettingsFile,
    root_source: &str,
    root_dot_path: &Path,
    cwd: &Path,
) -> Result<Option<types::ManifestGoal>> {
    let working_directory = project::resolve_working_directory(settings, cwd);

    // Precedence 1: CLI args (`--goal` / `--goal-file`). These are already
    // resolved to absolute paths by `overrides::goal_layer_from_args`.
    if let Some(resolved) = resolve_run_goal(args_layer, &working_directory)
        .context("failed to resolve --goal-file contents")?
    {
        return Ok(Some(resolved_goal_to_manifest(resolved)));
    }

    // Precedence 2: merged config `run.goal`. Config-sourced `goal.file`
    // paths were rewritten to absolute by `load_settings_path` at the
    // directory of the config file that declared them.
    if let Some(resolved) = resolve_run_goal(settings, &working_directory)
        .context("failed to resolve run.goal.file contents")?
    {
        return Ok(Some(resolved_goal_to_manifest(resolved)));
    }

    // Precedence 3: graph-level `goal` attribute in the DOT, with `@file`
    // sugar for workflow-colocated goal files.
    let graph = parser::parse(root_source)
        .map_err(|err| anyhow!("Failed to parse {}: {err}", root_dot_path.display()))?;
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
            path: Some(reference.to_string()),
            text: std::fs::read_to_string(&goal_path)
                .with_context(|| format!("Failed to read {}", goal_path.display()))?,
            type_: types::ManifestGoalType::Graph,
        }));
    }

    Ok(Some(types::ManifestGoal {
        path: None,
        text: goal.to_string(),
        type_: types::ManifestGoalType::Graph,
    }))
}

/// Translate a [`ResolvedRunGoal`] into the wire-level `ManifestGoal`
/// shape. Inline goals get `type = Value`; file-sourced goals keep their
/// absolute path as the `path` field and use `type = File`.
fn resolved_goal_to_manifest(resolved: ResolvedRunGoal) -> types::ManifestGoal {
    match resolved.source {
        ResolvedGoalSource::Inline => types::ManifestGoal {
            path: None,
            text: resolved.text,
            type_: types::ManifestGoalType::Value,
        },
        ResolvedGoalSource::File { path } => types::ManifestGoal {
            path: Some(path.to_string_lossy().into_owned()),
            text: resolved.text,
            type_: types::ManifestGoalType::File,
        },
    }
}

fn build_manifest_git(cwd: &Path) -> Option<types::ManifestGit> {
    let (origin_url, branch) = detect_repo_info(cwd).ok()?;
    let branch = branch?;
    let sha = head_sha(cwd).ok()?;
    let clean = sync_status(cwd, "origin", Some(&branch)) != GitSyncStatus::Dirty;
    Some(types::ManifestGit {
        branch,
        clean,
        origin_url: sanitize_origin_url(&origin_url),
        sha,
    })
}

fn sanitize_origin_url(origin_url: &str) -> String {
    fabro_github::normalize_repo_origin_url(origin_url)
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

fn to_logical_path(path: &Path, cwd: &Path) -> Result<PathBuf> {
    if let Ok(stripped) = path.strip_prefix(cwd) {
        return Ok(stripped.to_path_buf());
    }

    relative_path_from(path, cwd)
        .ok_or_else(|| anyhow!("Failed to compute logical path for {}", path.display()))
}

fn relative_path_from(path: &Path, base: &Path) -> Option<PathBuf> {
    let path_components = path.components().collect::<Vec<_>>();
    let base_components = base.components().collect::<Vec<_>>();
    if path_components.is_empty() || base_components.is_empty() {
        return None;
    }

    let mut common = 0;
    while common < path_components.len()
        && common < base_components.len()
        && path_components[common] == base_components[common]
    {
        common += 1;
    }

    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &path_components[common..] {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => relative.push(".."),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(relative)
}

fn logical_path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn manifest_args_is_empty(args: &types::ManifestArgs) -> bool {
    args.auto_approve.is_none()
        && args.dry_run.is_none()
        && args.label.is_empty()
        && args.model.is_none()
        && args.no_retro.is_none()
        && args.preserve_sandbox.is_none()
        && args.provider.is_none()
        && args.sandbox.is_none()
        && args.verbose.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(unsafe_code, clippy::allow_attributes)]
    fn build_manifest_bundles_imports_prompts_and_children() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join("fabro/workflows/demo");
        let child_dir = project.join("fabro/workflows/child");
        std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();
        std::fs::create_dir_all(workflow_dir.join("imports")).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(project.join("fabro.toml"), "_version = 1\n").unwrap();
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

        // Isolate from the developer's real ~/.fabro/settings.toml which may
        // still be in the legacy shape. Setting FABRO_CONFIG to a path inside
        // the test tempdir forces the loader to produce an empty user layer.
        let sandboxed_settings = temp.path().join("empty-settings.toml");
        std::fs::write(&sandboxed_settings, "_version = 1\n").unwrap();
        // SAFETY: single-threaded unit test body.
        unsafe {
            std::env::set_var("FABRO_CONFIG", &sandboxed_settings);
        }

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from("fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            args_layer: SettingsFile::default(),
            args: None,
            run_id: None,
        })
        .unwrap();

        // SAFETY: single-threaded unit test body.
        unsafe {
            std::env::remove_var("FABRO_CONFIG");
        }

        assert_eq!(
            built.manifest.target.path,
            "fabro/workflows/demo/workflow.fabro"
        );
        assert_eq!(built.manifest.workflows.len(), 2);
        let root = &built.manifest.workflows["fabro/workflows/demo/workflow.fabro"];
        assert!(
            root.files
                .contains_key("fabro/workflows/demo/prompts/goal.md")
        );
        assert!(
            root.files
                .contains_key("fabro/workflows/demo/prompts/plan.md")
        );
        assert!(
            root.files
                .contains_key("fabro/workflows/demo/imports/checks.fabro")
        );
        assert!(
            root.files
                .contains_key("fabro/workflows/demo/prompts/lint.md")
        );
        assert_eq!(built.manifest.goal.unwrap().text, "ship it");
        assert!(
            built
                .manifest
                .workflows
                .contains_key("fabro/workflows/child/workflow.fabro")
        );
    }

    /// A relative `[run.goal] file = "..."` declared in `fabro.toml` must
    /// resolve against the directory of `fabro.toml`, not against the
    /// invocation cwd. We exercise this by invoking from a subdirectory
    /// below the project root.
    #[test]
    #[allow(unsafe_code, clippy::allow_attributes)]
    fn build_manifest_resolves_relative_goal_file_in_project_config() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join("fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::create_dir_all(project.join("prompts")).unwrap();

        std::fs::write(
            project.join("fabro.toml"),
            r#"_version = 1

[run.goal]
file = "prompts/goal.md"
"#,
        )
        .unwrap();
        std::fs::write(project.join("prompts/goal.md"), "ship from project root").unwrap();

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

        let sandboxed_settings = temp.path().join("empty-settings.toml");
        std::fs::write(&sandboxed_settings, "_version = 1\n").unwrap();
        // SAFETY: single-threaded unit test body.
        unsafe {
            std::env::set_var("FABRO_CONFIG", &sandboxed_settings);
        }

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from("fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            args_layer: SettingsFile::default(),
            args: None,
            run_id: None,
        })
        .unwrap();

        // SAFETY: single-threaded unit test body.
        unsafe {
            std::env::remove_var("FABRO_CONFIG");
        }

        let goal = built.manifest.goal.expect("manifest goal should be set");
        assert_eq!(goal.text, "ship from project root");
        assert_eq!(goal.type_, types::ManifestGoalType::File);
        let resolved = goal.path.expect("file goal must carry a path");
        let expected = project.join("prompts").join("goal.md");
        assert_eq!(PathBuf::from(resolved), expected);
    }

    /// A relative `[run.goal] file = "..."` declared in `workflow.toml`
    /// must resolve against the directory of `workflow.toml`, not against
    /// the invocation cwd or project root.
    #[test]
    #[allow(unsafe_code, clippy::allow_attributes)]
    fn build_manifest_resolves_relative_goal_file_in_workflow_config() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join("fabro/workflows/demo");
        std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();

        std::fs::write(project.join("fabro.toml"), "_version = 1\n").unwrap();
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

        let sandboxed_settings = temp.path().join("empty-settings.toml");
        std::fs::write(&sandboxed_settings, "_version = 1\n").unwrap();
        // SAFETY: single-threaded unit test body.
        unsafe {
            std::env::set_var("FABRO_CONFIG", &sandboxed_settings);
        }

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from("fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            args_layer: SettingsFile::default(),
            args: None,
            run_id: None,
        })
        .unwrap();

        // SAFETY: single-threaded unit test body.
        unsafe {
            std::env::remove_var("FABRO_CONFIG");
        }

        let goal = built.manifest.goal.expect("manifest goal should be set");
        assert_eq!(goal.text, "ship from workflow dir");
        assert_eq!(goal.type_, types::ManifestGoalType::File);
        let resolved = goal.path.expect("file goal must carry a path");
        let expected = workflow_dir.join("prompts").join("goal.md");
        assert_eq!(PathBuf::from(resolved), expected);
    }
}
