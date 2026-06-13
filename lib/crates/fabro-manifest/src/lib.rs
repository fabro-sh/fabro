#![expect(
    clippy::disallowed_methods,
    reason = "CLI manifest builder: sync file I/O building install manifests"
)]

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use fabro_api::types;
use fabro_config::project::{self, WorkflowLocation, discover_project_config};
use fabro_config::run::{resolve_run_goal_from_layer, resolve_run_goal_from_namespace};
use fabro_config::{
    CliLayer, EnvironmentDockerfileLayer, EnvironmentImageLayer, EnvironmentLayer,
    EnvironmentLifecycleLayer, MergeMap, ReplaceMap, RunEnvironmentLayer, RunExecutionLayer,
    RunGoalLayer, RunLayer, RunModelLayer, SettingsLayer, WorkflowSettingsBuilder,
};
use fabro_graphviz::graph::AttrValue;
use fabro_graphviz::parser;
use fabro_template::{
    BundleTemplateStore, FilesystemTemplateStore, RecordingTemplateStore, TemplateContext,
    TemplateRenderMode, TemplateSource, discover_static_dependency_closure, render_source,
};
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{ApprovalMode, ResolvedGoalSource, ResolvedRunGoal, RunMode};
use fabro_types::{
    DirtyStatus, GitContext, ManifestPath, PreRunPushOutcome, RunId, WorkflowSettings,
};
use fabro_workflow::git::{
    GitSyncStatus, branch_needs_push, head_sha, push_branch_noninteractive, sync_status,
};
use fabro_workflow::static_reference::{
    AttributeScope, ReferenceKind, reference_kind_for_attribute,
};

#[derive(Debug, Default)]
pub struct ManifestBuildInput {
    pub workflow:             PathBuf,
    pub cwd:                  PathBuf,
    pub run_overrides:        Option<RunLayer>,
    pub cli_overrides:        Option<CliLayer>,
    pub input_overrides:      HashMap<String, toml::Value>,
    pub args:                 Option<types::ManifestArgs>,
    pub run_id:               Option<RunId>,
    pub environment_defaults: MergeMap<EnvironmentLayer>,
    /// Path to the user settings file (for inclusion in
    /// `RunManifest.configs`). `None` skips the user config entry.
    pub user_settings_path:   Option<PathBuf>,
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
    pub environment:      Option<&'a str>,
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
    let environment = (input.environment.is_some()
        || input.docker_image.is_some()
        || input.preserve_sandbox.is_some())
    .then(|| RunEnvironmentLayer {
        id: input.environment.map(ToOwned::to_owned),
        image: input.docker_image.map(|image| EnvironmentImageLayer {
            docker: Some(image.to_string()),
            ..EnvironmentImageLayer::default()
        }),
        lifecycle: input
            .preserve_sandbox
            .map(|preserve| EnvironmentLifecycleLayer {
                preserve: Some(preserve),
                ..EnvironmentLifecycleLayer::default()
            }),
        ..RunEnvironmentLayer::default()
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
        environment,
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
        || run.environment.is_some()
        || run.execution.is_some())
    .then_some(run)
}

struct CollectContext<'a> {
    cwd:               &'a Path,
    inputs:            HashMap<String, toml::Value>,
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

    let mut workflow_settings_builder = WorkflowSettingsBuilder::new()
        .server_manifest_defaults(RunLayer::default(), input.environment_defaults.clone());
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
        inputs:            workflow_settings.run.inputs.clone(),
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

    let goal = resolve_manifest_goal(
        input.run_overrides.as_ref(),
        &workflow_settings,
        &root_source,
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
    let graph = parser::parse(&workflow.source).map_err(|err| {
        anyhow!(
            "Failed to parse {}: {err}",
            workflow.absolute_dot_path.display()
        )
    })?;
    let workflow_base_dir = workflow
        .absolute_dot_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let workflow_template_root = manifest_parent_or_dot(&workflow.dot_path)?;

    if let Some(goal_ref) = graph.attrs.get("goal").and_then(AttrValue::as_str) {
        if goal_ref.starts_with('@') {
            let bundled = collect_bundled_file(
                files,
                workflow_base_dir,
                context.cwd,
                goal_ref.trim_start_matches('@'),
                types::ManifestFileRefType::FileInline,
                manifest_attr_reference_kind(AttributeScope::Graph, "goal", goal_ref)?,
                Some(workflow.dot_path.clone()),
            )?;
            let source = std::fs::read_to_string(&bundled.absolute_path)
                .with_context(|| format!("Failed to read {}", bundled.absolute_path.display()))?;
            let template_root =
                template_root_for_bundled_file(&bundled.path, &workflow_template_root)?;
            collect_template_include_files(
                files,
                context.cwd,
                TemplateSource::new(bundled.path.clone(), template_root, source),
                Some(&bundled.path),
                &context.inputs,
            )?;
        } else {
            collect_template_include_files(
                files,
                context.cwd,
                TemplateSource::new(
                    workflow.dot_path.clone(),
                    workflow_template_root.clone(),
                    goal_ref.to_owned(),
                ),
                Some(&workflow.dot_path),
                &context.inputs,
            )?;
        }
    }

    for node in graph.nodes.values() {
        if let Some(prompt_ref) = node.attrs.get("prompt").and_then(AttrValue::as_str) {
            if prompt_ref.starts_with('@') {
                let bundled = collect_bundled_file(
                    files,
                    workflow_base_dir,
                    context.cwd,
                    prompt_ref.trim_start_matches('@'),
                    types::ManifestFileRefType::FileInline,
                    manifest_attr_reference_kind(AttributeScope::Node, "prompt", prompt_ref)?,
                    Some(workflow.dot_path.clone()),
                )?;
                let source =
                    std::fs::read_to_string(&bundled.absolute_path).with_context(|| {
                        format!("Failed to read {}", bundled.absolute_path.display())
                    })?;
                let template_root =
                    template_root_for_bundled_file(&bundled.path, &workflow_template_root)?;
                collect_template_include_files(
                    files,
                    context.cwd,
                    TemplateSource::new(bundled.path.clone(), template_root, source),
                    Some(&bundled.path),
                    &context.inputs,
                )?;
            } else {
                collect_template_include_files(
                    files,
                    context.cwd,
                    TemplateSource::new(
                        workflow.dot_path.clone(),
                        workflow_template_root.clone(),
                        prompt_ref.to_owned(),
                    ),
                    Some(&workflow.dot_path),
                    &context.inputs,
                )?;
            }
        }

        if let Some(import_ref) = node.attrs.get("import").and_then(AttrValue::as_str) {
            let imported = collect_bundled_file(
                files,
                workflow_base_dir,
                context.cwd,
                import_ref,
                types::ManifestFileRefType::Import,
                manifest_attr_reference_kind(AttributeScope::Node, "import", import_ref)?,
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
            .and_then(AttrValue::as_str)
        {
            manifest_attr_reference_kind(AttributeScope::Node, "stack.child_workflow", child_ref)?
                .validate(child_ref)
                .map_err(anyhow::Error::new)?;
            collect_workflow_entry(context, Path::new(child_ref), workflow_base_dir)?;
        }
    }

    Ok(())
}

fn collect_template_include_files(
    files: &mut HashMap<String, types::ManifestFileEntry>,
    cwd: &Path,
    source: TemplateSource,
    from: Option<&ManifestPath>,
    inputs: &HashMap<String, toml::Value>,
) -> Result<()> {
    let source_path = source.path.clone();
    let store = FilesystemTemplateStore::new(cwd.to_path_buf());
    let closure = discover_static_dependency_closure([source], &store)
        .map_err(|err| anyhow!("failed to discover template dependencies: {err}"))?;
    verify_recorded_template_dependencies(&source_path, &closure, files, from, inputs)?;

    for (path, source) in closure.sources {
        if path == source_path {
            continue;
        }
        let key = path.to_string();
        files
            .entry(key)
            .or_insert_with(|| types::ManifestFileEntry {
                content: source.content,
                ref_:    types::ManifestFileRef {
                    from:     from.map(std::string::ToString::to_string),
                    original: path.to_string(),
                    type_:    types::ManifestFileRefType::FileInline,
                },
            });
    }
    Ok(())
}

fn template_root_for_bundled_file(
    path: &ManifestPath,
    workflow_template_root: &ManifestPath,
) -> Result<ManifestPath> {
    if manifest_path_is_within_root(path, workflow_template_root) {
        Ok(workflow_template_root.clone())
    } else {
        manifest_parent_or_dot(path)
    }
}

fn manifest_path_is_within_root(path: &ManifestPath, root: &ManifestPath) -> bool {
    if root.as_path().as_os_str().is_empty() {
        return !matches!(
            path.as_path().components().next(),
            Some(Component::ParentDir)
        );
    }
    path.starts_with(root)
}

fn verify_recorded_template_dependencies(
    source_path: &ManifestPath,
    closure: &fabro_template::TemplateDependencyClosure,
    files: &HashMap<String, types::ManifestFileEntry>,
    from: Option<&ManifestPath>,
    inputs: &HashMap<String, toml::Value>,
) -> Result<()> {
    let Some(source) = closure.sources.get(source_path) else {
        return Ok(());
    };
    let mut bundled_files = closure
        .sources
        .iter()
        .map(|(path, source)| (path.clone(), source.content.clone()))
        .collect::<HashMap<_, _>>();
    for (path, entry) in files {
        if let Some(path) = ManifestPath::from_wire(path) {
            bundled_files.insert(path, entry.content.clone());
        }
    }
    let allowed = bundled_files.keys().cloned().collect();
    let store =
        RecordingTemplateStore::with_allowed(BundleTemplateStore::new(bundled_files), allowed);
    let ctx = TemplateContext::for_input_scan(inputs.clone());
    render_source(source, &ctx, Arc::new(store), TemplateRenderMode::Lenient).with_context(
        || {
            let from =
                from.map_or_else(|| source_path.to_string(), std::string::ToString::to_string);
            format!("failed to verify template dependencies for {from}")
        },
    )?;
    Ok(())
}

fn manifest_attr_reference_kind(
    scope: AttributeScope,
    key: &str,
    value: &str,
) -> Result<ReferenceKind> {
    reference_kind_for_attribute(scope, key, value)
        .ok_or_else(|| anyhow!("unsupported manifest reference attribute: {key}={value}"))
}

fn collect_config_dockerfile(
    cwd: &Path,
    config_path: &ManifestPath,
    source: &str,
    files: &mut HashMap<String, types::ManifestFileEntry>,
) -> Result<()> {
    let layer = source
        .parse::<SettingsLayer>()
        .context("Failed to parse run config TOML")?;
    let absolute_config_path = cwd.join(config_path.as_path());
    let base_dir = absolute_config_path
        .parent()
        .unwrap_or_else(|| Path::new("."));

    for environment in layer.environments.values() {
        collect_environment_dockerfile(
            files,
            base_dir,
            cwd,
            config_path,
            environment.image.as_ref(),
        )?;
    }
    if let Some(run_environment) = layer.run.as_ref().and_then(|run| run.environment.as_ref()) {
        collect_environment_dockerfile(
            files,
            base_dir,
            cwd,
            config_path,
            run_environment.image.as_ref(),
        )?;
    }
    Ok(())
}

fn collect_environment_dockerfile(
    files: &mut HashMap<String, types::ManifestFileEntry>,
    base_dir: &Path,
    cwd: &Path,
    config_path: &ManifestPath,
    image: Option<&EnvironmentImageLayer>,
) -> Result<()> {
    let dockerfile = image.and_then(|image| image.dockerfile.as_ref());
    let Some(EnvironmentDockerfileLayer::Path { path }) = dockerfile else {
        return Ok(());
    };
    collect_bundled_file(
        files,
        base_dir,
        cwd,
        path,
        types::ManifestFileRefType::Dockerfile,
        ReferenceKind::Dockerfile,
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
    reference_kind: ReferenceKind,
    from: Option<ManifestPath>,
) -> Result<BundledFile> {
    reference_kind
        .validate(reference)
        .map_err(anyhow::Error::new)?;

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
        ReferenceKind::GraphGoalFile
            .validate(reference)
            .map_err(anyhow::Error::new)?;
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

#[expect(
    clippy::disallowed_methods,
    reason = "raw source is today's behavior; run.scm.owner/repository are slated for demotion to plain String in the \
              interpolation unification (D2)"
)]
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

fn manifest_parent_or_dot(path: &ManifestPath) -> Result<ManifestPath> {
    let parent = path.parent_or_dot().to_string_lossy();
    ManifestPath::from_wire(&parent)
        .ok_or_else(|| anyhow!("invalid manifest parent path for {path}: {parent}"))
}

pub fn manifest_args_is_empty(args: &types::ManifestArgs) -> bool {
    args.auto_approve.is_none()
        && args.dry_run.is_none()
        && args.label.is_empty()
        && args.model.is_none()
        && args.preserve_sandbox.is_none()
        && args.provider.is_none()
        && args.environment.is_none()
        && args.docker_image.is_none()
        && args.input.is_empty()
        && args.verbose.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_environment_defaults() -> MergeMap<EnvironmentLayer> {
        MergeMap::from(std::collections::HashMap::from([(
            "default".to_string(),
            EnvironmentLayer {
                provider: Some("local".to_string()),
                ..EnvironmentLayer::default()
            },
        )]))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test asserts the raw template source"
    )]
    #[test]
    fn build_run_overrides_sets_common_cli_and_mcp_layers() {
        let overrides = build_run_overrides(RunOverrideInput {
            goal:             Some("ship it"),
            model:            Some("gpt-5.4-mini"),
            provider:         Some("openai"),
            environment:      Some("local"),
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
            overrides.environment.as_ref().unwrap().id.as_deref(),
            Some("local")
        );
        assert_eq!(
            overrides
                .environment
                .as_ref()
                .unwrap()
                .lifecycle
                .as_ref()
                .unwrap()
                .preserve,
            Some(true)
        );
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
            environment_defaults: test_environment_defaults(),
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
    fn build_manifest_bundles_static_minijinja_includes_from_prompts_and_goals() {
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
                graph [goal="@prompts/goal.md"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                file_prompt [prompt="@prompts/plan.md"]
                inline_prompt [prompt="{% include 'inline.tpl.md' %}"]
                start -> file_prompt -> inline_prompt -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("prompts/goal.md"),
            r#"{% include "goal.tpl.md" %}"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/goal.tpl.md"), "ship it").unwrap();
        std::fs::write(
            workflow_dir.join("prompts/plan.md"),
            r#"{% include "plan.tpl.md" %}"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/plan.tpl.md"), "plan it").unwrap();
        std::fs::write(workflow_dir.join("inline.tpl.md"), "inline it").unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap();

        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/prompts/goal.tpl.md")
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/prompts/plan.tpl.md")
        );
        assert!(
            root.files
                .contains_key(".fabro/workflows/demo/inline.tpl.md")
        );
    }

    #[test]
    fn build_manifest_bundles_static_minijinja_includes_from_all_branches_and_macros() {
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
                graph [goal="ship"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                work [prompt="@prompts/plan.md"]
                start -> work -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("prompts/plan.md"),
            r#"{% if inputs.use_a %}{% include "a.md" %}{% else %}{% include "b.md" %}{% endif %}
{% from "helpers.md" import render_advanced_prompt %}"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/a.md"), "A").unwrap();
        std::fs::write(workflow_dir.join("prompts/b.md"), "B").unwrap();
        std::fs::write(
            workflow_dir.join("prompts/helpers.md"),
            r#"{% macro render_advanced_prompt() %}{% include "advanced.md" %}{% endmacro %}"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/advanced.md"), "advanced").unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap();

        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        for path in [
            ".fabro/workflows/demo/prompts/a.md",
            ".fabro/workflows/demo/prompts/b.md",
            ".fabro/workflows/demo/prompts/helpers.md",
            ".fabro/workflows/demo/prompts/advanced.md",
        ] {
            assert!(root.files.contains_key(path), "missing {path}");
        }
    }

    #[test]
    fn build_manifest_rejects_dynamic_minijinja_include_discovery() {
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
                graph [goal="ship"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                work [prompt="@prompts/plan.md"]
                start -> work -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("prompts/plan.md"),
            r"{% include inputs.partial %}",
        )
        .unwrap();

        let err = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap_err();

        assert!(
            err.to_string().contains("dynamic template dependency"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn build_manifest_accepts_project_environment_catalog_definitions() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();

        std::fs::write(
            project.join(".fabro/project.toml"),
            r#"_version = 1

[run.environment]
id = "daytona"

[environments.daytona]
provider = "local"
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
            cwd: project.to_path_buf(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .expect("project environment catalog definitions should be accepted");

        assert!(built.manifest.configs.iter().any(|config| {
            config.type_ == types::ManifestConfigType::Project
                && config
                    .source
                    .as_deref()
                    .is_some_and(|source| source.contains("[environments.daytona]"))
        }));
    }

    #[test]
    fn build_manifest_rejects_templated_file_references() {
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
                start -> plan -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(workflow_dir.join("prompts/plan.md"), "plan it").unwrap();

        let err = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("templates are not supported in file inline references"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn build_manifest_rejects_templated_import_reference() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(workflow_dir.join("imports")).unwrap();
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
                imported [import="./imports/{{ inputs.import_file }}"]
                exit [shape=Msquare]
                start -> imported -> exit
            }"#,
        )
        .unwrap();

        let err = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            input_overrides: HashMap::from([(
                "import_file".to_string(),
                toml::Value::String("checks.fabro".to_string()),
            )]),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("templates are not supported in import references"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn build_manifest_rejects_templated_child_workflow_reference() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        std::fs::create_dir_all(&workflow_dir).unwrap();
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
                child [shape=house, stack.child_workflow="../{{ inputs.child_workflow }}/workflow.fabro"]
                exit [shape=Msquare]
                start -> child -> exit
            }"#,
        )
        .unwrap();

        let err = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            input_overrides: HashMap::from([(
                "child_workflow".to_string(),
                toml::Value::String("child".to_string()),
            )]),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("templates are not supported in child workflow references"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn build_manifest_rejects_templated_graph_goal_file_reference() {
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

        let err = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            input_overrides: HashMap::from([(
                "goal_file".to_string(),
                toml::Value::String("goal.md".to_string()),
            )]),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("templates are not supported in graph goal file references"),
            "unexpected error: {err:#}"
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
            environment_defaults: test_environment_defaults(),
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
            environment_defaults: test_environment_defaults(),
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
            environment_defaults: test_environment_defaults(),
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
            environment_defaults: test_environment_defaults(),
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
                    environment_defaults: test_environment_defaults(),
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
