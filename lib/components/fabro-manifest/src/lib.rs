#![expect(
    clippy::disallowed_methods,
    reason = "CLI manifest builder: sync file I/O building install manifests"
)]

mod local_workflow_package;
mod workflow_bundler;
mod workflow_version_collector;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use fabro_api::types;
use fabro_config::project::{self, WorkflowLocation, discover_project_config};
use fabro_config::run::{resolve_run_goal_from_layer, resolve_run_goal_from_namespace};
use fabro_config::{
    CliLayer, EnvironmentLayer, EnvironmentLifecycleLayer, MergeMap, ReplaceMap,
    RunEnvironmentLayer, RunExecutionLayer, RunGoalLayer, RunLayer, RunModelLayer,
    WorkflowSettingsBuilder,
};
use fabro_graphviz::graph::AttrValue;
use fabro_graphviz::parser;
use fabro_template::validate_static_reference;
use fabro_types::graph::ReferenceKind;
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{ApprovalMode, ResolvedGoalSource, ResolvedRunGoal, RunMode};
use fabro_types::{
    DirtyStatus, GitContext, GitHubRepositorySlug, GitRunTarget, ManifestPath, RunTarget,
    WorkflowSettings,
};
use fabro_workflow::git::{self, GitSyncStatus};

pub use crate::local_workflow_package::{
    LocalWorkflowPackageError, ResolvedLocalWorkflowPackage, resolve_local_workflow_package,
};
use crate::workflow_bundler::WorkflowBundler;
pub use crate::workflow_version_collector::{
    CollectedWorkflowClosure, WorkflowVersionCollectError, collect_workflow_versions,
};

#[derive(Debug, Default)]
pub struct ManifestBuildInput {
    pub workflow:             PathBuf,
    pub cwd:                  PathBuf,
    pub run_overrides:        Option<RunLayer>,
    pub cli_overrides:        Option<CliLayer>,
    pub input_overrides:      HashMap<String, toml::Value>,
    pub args:                 Option<types::ManifestArgs>,
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
        provider:  input.provider.map(String::from),
        name:      input.model.map(String::from),
        fallbacks: MergeMap::default(),
        controls:  None,
    });
    let environment =
        (input.environment.is_some() || input.preserve_sandbox.is_some()).then(|| {
            RunEnvironmentLayer {
                id: input.environment.map(ToOwned::to_owned),
                lifecycle: input
                    .preserve_sandbox
                    .map(|preserve| EnvironmentLifecycleLayer {
                        preserve: Some(preserve),
                        ..EnvironmentLifecycleLayer::default()
                    }),
                ..RunEnvironmentLayer::default()
            }
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

pub fn build_run_manifest(input: ManifestBuildInput) -> Result<BuiltManifest> {
    let root_location = resolve_existing_workflow_location(&input.workflow, &input.cwd)?;
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
    let project_config_input = project_config_source
        .as_ref()
        .map(|(_, path, source)| (path, source.as_str()));
    let workflows = WorkflowBundler::new(&input.cwd, &workflow_settings.run.inputs)
        .bundle(&input.workflow, project_config_input)?;
    let root_source = workflows
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
    if let Some(path) = input.user_settings_path.filter(|path| path.is_file()) {
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
    let git = build_legacy_git_context(&working_directory, configured_repo_origin_url.as_deref());
    let args = input.args.filter(|args| !manifest_args_is_empty(args));

    Ok(BuiltManifest {
        manifest: types::RunManifest {
            args,
            configs,
            cwd: input.cwd.display().to_string(),
            git,
            goal,
            parent_id: None,
            title: None,
            target: types::ManifestTarget { path: target_key },
            version: 1,
            workflows,
        },
        target_path,
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
        validate_static_reference(reference, ReferenceKind::GraphGoalFile)
            .map_err(anyhow::Error::new)?;
        let goal_path = normalize_absolute_path(
            root_dot_path.parent().unwrap_or_else(|| Path::new(".")),
            reference,
        )
        .ok_or_else(|| anyhow!("unsupported manifest goal reference: {reference}"))?;
        return Ok(Some(types::ManifestGoal {
            text:  std::fs::read_to_string(&goal_path)
                .with_context(|| format!("Failed to read {}", goal_path.display()))?,
            type_: types::ManifestGoalType::Graph,
        }));
    }

    Ok(Some(types::ManifestGoal {
        text:  goal.to_string(),
        type_: types::ManifestGoalType::Graph,
    }))
}

/// Translate a [`ResolvedRunGoal`] into the wire-level `ManifestGoal`
/// shape. Inline goals get `type = Value`; file-sourced goals carry their
/// already-resolved contents with `type = File`.
fn resolved_goal_to_manifest(resolved: ResolvedRunGoal) -> types::ManifestGoal {
    match resolved.source {
        ResolvedGoalSource::Inline => types::ManifestGoal {
            text:  resolved.text,
            type_: types::ManifestGoalType::Value,
        },
        ResolvedGoalSource::File { .. } => types::ManifestGoal {
            text:  resolved.text,
            type_: types::ManifestGoalType::File,
        },
    }
}

/// Facts observed from one usable attached local Git checkout.
///
/// The optional target is absent when the checkout cannot be represented as a
/// valid GitHub run target. Its SHA is present only when a successful push or
/// a direct query of the remote proves that exact commit is available. The
/// legacy projection retains the historical local SHA and normalized-origin
/// behavior independently.
#[derive(Clone, Debug)]
pub struct GitRunTargetObservation {
    pub run_target:         Option<GitRunTarget>,
    pub legacy_git_context: GitContext,
}

/// Observe Git facts without choosing an environment or a non-Git target.
///
/// Outer `None` means `repo_path` is not a usable attached checkout. A
/// returned observation with no target means Git facts were available but the
/// effective origin or attached branch cannot be represented by a valid
/// GitHub run target. For a valid target, this operation may contact the
/// remote and may make one noninteractive best-effort push of the attached
/// branch so clone-based execution can resolve the observed commit.
#[must_use]
pub fn observe_git_run_target(
    repo_path: &Path,
    configured_repo_origin_url: Option<&str>,
) -> Option<GitRunTargetObservation> {
    let local = inspect_local_git(repo_path, configured_repo_origin_url)?;
    let legacy_git_context = local.legacy_git_context;
    let mut run_target =
        github_run_target(&legacy_git_context.origin_url, &legacy_git_context.branch);
    if let Some(target) = run_target.as_mut() {
        let publish_status = publish_manifest_branch_best_effort(
            repo_path,
            &legacy_git_context.branch,
            local.push_origin_url.as_deref(),
            configured_repo_origin_url,
        );
        target.sha = remotely_available_sha(
            repo_path,
            &legacy_git_context.branch,
            legacy_git_context.sha.as_deref(),
            publish_status,
        );
    }

    Some(GitRunTargetObservation {
        run_target,
        legacy_git_context,
    })
}

struct LocalGitObservation {
    push_origin_url:    Option<String>,
    legacy_git_context: GitContext,
}

fn inspect_local_git(
    repo_path: &Path,
    configured_repo_origin_url: Option<&str>,
) -> Option<LocalGitObservation> {
    let ManifestRepoInfo {
        origin_url,
        push_origin_url,
        branch,
        sha,
    } = detect_manifest_repo_info(repo_path)?;
    let dirty = match git::sync_status(repo_path, "origin", Some(&branch)) {
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

    Some(LocalGitObservation {
        push_origin_url,
        legacy_git_context: GitContext {
            origin_url: repo_origin_url,
            branch,
            sha,
            dirty,
        },
    })
}

fn build_legacy_git_context(
    repo_path: &Path,
    configured_repo_origin_url: Option<&str>,
) -> Option<GitContext> {
    let local = inspect_local_git(repo_path, configured_repo_origin_url)?;
    publish_manifest_branch_best_effort(
        repo_path,
        &local.legacy_git_context.branch,
        local.push_origin_url.as_deref(),
        configured_repo_origin_url,
    );
    Some(local.legacy_git_context)
}

fn github_run_target(origin_url: &str, branch: &str) -> Option<GitRunTarget> {
    let (owner, repository) = fabro_github::parse_github_owner_repo(origin_url).ok()?;
    let slug = GitHubRepositorySlug::try_new(&format!("{owner}/{repository}"))?;
    let validated = RunTarget::Git(GitRunTarget {
        repo:   slug.to_string(),
        branch: branch.to_owned(),
        tag:    None,
        sha:    None,
    })
    .validate()
    .ok()?;
    let RunTarget::Git(target) = validated.target else {
        unreachable!("a validated Git target must remain a Git target")
    };
    Some(target)
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
    let owner = scm.owner.as_deref()?;
    let repository = scm.repository.as_deref()?;
    if owner.trim().is_empty() || repository.trim().is_empty() {
        return None;
    }
    let origin = format!("https://github.com/{owner}/{repository}");
    let normalized = fabro_github::normalize_repo_origin_url(&origin);
    (!normalized.is_empty()).then_some(normalized)
}

struct ManifestRepoInfo {
    /// The `origin` URL as libgit2 reports it (after `insteadOf` rewrites).
    origin_url:      Option<String>,
    /// The raw configured `remote.origin.url`, used to compare repository
    /// identity before pushing.
    push_origin_url: Option<String>,
    branch:          String,
    sha:             Option<String>,
}

fn detect_manifest_repo_info(repo_path: &Path) -> Option<ManifestRepoInfo> {
    let repo = git2::Repository::discover(repo_path).ok()?;
    if repo.is_bare() {
        return None;
    }
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    let branch = head.shorthand().map(ToOwned::to_owned)?;
    let sha = head.target().map(|oid| oid.to_string());
    let origin_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|remote| remote.url().map(ToOwned::to_owned));
    // Keep the legacy observed URL above, but compare configured repository
    // identity against the remote's raw config bytes. This lets a repository-
    // local `url.*.insteadOf` redirect the existing push safely without making
    // the rewrite target look like a different repository.
    let push_origin_url = repo
        .config()
        .ok()
        .and_then(|config| config.get_string("remote.origin.url").ok())
        .or_else(|| origin_url.clone());
    Some(ManifestRepoInfo {
        origin_url,
        push_origin_url,
        branch,
        sha,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BranchPublishStatus {
    TrackingRefMatches,
    Pushed,
    Unavailable,
}

/// Best-effort publication of the local branch so clone-based execution can
/// see local commits. A failed push must not fail manifest creation, and the
/// discarded push error may contain raw Git stderr, so it is deliberately
/// neither returned nor logged here.
fn publish_manifest_branch_best_effort(
    repo_path: &Path,
    branch: &str,
    origin_url: Option<&str>,
    configured_repo_origin_url: Option<&str>,
) -> BranchPublishStatus {
    let Some(origin_url) = origin_url else {
        return BranchPublishStatus::Unavailable;
    };

    if let Some(repo_origin_url) = configured_repo_origin_url
        .map(fabro_github::normalize_repo_origin_url)
        .filter(|url| !url.is_empty())
    {
        let remote = fabro_github::normalize_repo_origin_url(origin_url);
        if remote != repo_origin_url {
            return BranchPublishStatus::Unavailable;
        }
    }

    if !git::branch_needs_push(repo_path, "origin", branch) {
        return BranchPublishStatus::TrackingRefMatches;
    }

    if git::push_branch_noninteractive(repo_path, "origin", branch).is_ok() {
        BranchPublishStatus::Pushed
    } else {
        BranchPublishStatus::Unavailable
    }
}

fn remotely_available_sha(
    repo_path: &Path,
    branch: &str,
    local_sha: Option<&str>,
    publish_status: BranchPublishStatus,
) -> Option<String> {
    let local_sha = local_sha?;
    match publish_status {
        BranchPublishStatus::Pushed => Some(local_sha.to_owned()),
        BranchPublishStatus::TrackingRefMatches => {
            git::remote_branch_sha_noninteractive(repo_path, "origin", branch)
                .ok()
                .flatten()
                .filter(|remote_sha| remote_sha == local_sha)
                .map(|_| local_sha.to_owned())
        }
        BranchPublishStatus::Unavailable => None,
    }
}

/// Resolve a workflow reference and reject it when neither its config nor
/// its graph exists on disk.
/// A missing workflow surfaces as `fabro_config::Error::WorkflowNotFound`.
#[expect(
    clippy::result_large_err,
    reason = "callers match on the concrete config error to classify missing workflows"
)]
fn resolve_existing_workflow_location(
    workflow: &Path,
    cwd: &Path,
) -> Result<WorkflowLocation, fabro_config::Error> {
    let location = WorkflowLocation::resolve(workflow, cwd)?;
    if location.toml.is_none() && !location.graph.is_file() {
        return Err(fabro_config::Error::WorkflowNotFound(
            location.graph.display().to_string(),
        ));
    }
    Ok(location)
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
        && args.environment.is_none()
        && args.input.is_empty()
        && args.verbose.is_none()
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use std::path::Path;

    pub(crate) fn write_file(path: &Path, source: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory should be created");
        }
        std::fs::write(path, source).expect("fixture file should be written");
    }
}

#[cfg(test)]
mod tests {
    use fabro_workflow::git::head_sha;

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

    fn assert_manifest_bundles_output_schema_file(node_attributes: &str) {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workflow_dir = project.join(".fabro/workflows/demo");
        let schema_source = r#"{"type":"object","required":["ok"]}"#;
        std::fs::create_dir_all(workflow_dir.join("schemas")).unwrap();
        std::fs::write(project.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        std::fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("workflow.fabro"),
            format!(
                r#"digraph Demo {{
                    start [shape=Mdiamond]
                    output [{node_attributes}, output_schema="@schemas/output.schema.json"]
                    exit [shape=Msquare]
                    start -> output -> exit
                }}"#
            ),
        )
        .unwrap();
        std::fs::write(
            workflow_dir.join("schemas/output.schema.json"),
            schema_source,
        )
        .unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: project.to_path_buf(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap();

        let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
        let schema = root
            .files
            .get(".fabro/workflows/demo/schemas/output.schema.json")
            .expect("output_schema file should be bundled");
        assert_eq!(schema.content, schema_source);
    }

    #[test]
    fn build_manifest_characterizes_the_complete_legacy_projection() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let root = project.join(".fabro/workflows/root");
        let child = project.join(".fabro/workflows/child");
        let user_config_path = temp.path().join("home/.fabro/config.toml");
        let project_config = r#"_version = 1

[environments.project]
provider = "docker"

[environments.project.image]
dockerfile = { path = "Project.Dockerfile" }
"#;
        let root_config = r#"_version = 1

[workflow]
graph = "workflow.fabro"
"#;
        let child_config = root_config;
        let user_config = "_version = 1\n";
        let root_graph = r#"digraph Root {
            graph [goal="@goals/goal.md"]
            start [shape=Mdiamond]
            prompt [prompt="@prompts/plan.md"]
            schema [type="agent", prompt="schema", output_schema="@schemas/output.json"]
            imported [import="imports/shared.fabro"]
            child [shape=house, stack.child_workflow="../child/workflow.fabro"]
            exit [shape=Msquare]
            start -> prompt -> schema -> imported -> child -> exit
        }"#;
        let child_graph =
            "digraph Child { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
        let imported_graph = r#"digraph Shared {
            start [shape=Mdiamond]
            shared [prompt="@../prompts/shared.md"]
            exit [shape=Msquare]
            start -> shared -> exit
        }"#;
        let plan_prompt = "{% include \"partial.md\" %}\n{% from \"helpers.md\" import render %}";
        let helpers = "{% macro render() %}{% include \"deep.md\" %}{% endmacro %}";
        let output_schema = r#"{"type":"object"}"#;
        let write = test_fixtures::write_file;
        write(&project.join(".fabro/project.toml"), project_config);
        write(&project.join(".fabro/Project.Dockerfile"), "FROM project\n");
        write(&user_config_path, user_config);
        write(&root.join("workflow.toml"), root_config);
        write(&root.join("workflow.fabro"), root_graph);
        write(&root.join("goals/goal.md"), "ship it\n");
        write(&root.join("prompts/plan.md"), plan_prompt);
        write(&root.join("prompts/partial.md"), "partial\n");
        write(&root.join("prompts/helpers.md"), helpers);
        write(&root.join("prompts/deep.md"), "deep\n");
        write(&root.join("prompts/shared.md"), "shared\n");
        write(&root.join("schemas/output.json"), output_schema);
        write(&root.join("imports/shared.fabro"), imported_graph);
        write(&child.join("workflow.toml"), child_config);
        write(&child.join("workflow.fabro"), child_graph);

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/root/workflow.toml"),
            cwd: project.clone(),
            input_overrides: HashMap::from([("feature".to_owned(), toml::Value::Boolean(true))]),
            args: Some(types::ManifestArgs {
                dry_run: Some(true),
                input: vec!["feature=true".to_owned()],
                label: vec!["suite=characterization".to_owned()],
                ..types::ManifestArgs::default()
            }),
            environment_defaults: test_environment_defaults(),
            user_settings_path: Some(user_config_path),
            ..ManifestBuildInput::default()
        })
        .unwrap();

        let mut actual = serde_json::to_value(&built.manifest).unwrap();
        actual["cwd"] = serde_json::json!("<cwd>");
        actual["configs"][0]["path"] = serde_json::json!("<project-config>");
        actual["configs"][1]["path"] = serde_json::json!("<user-config>");
        fabro_test::fabro_json_snapshot!(sorted_json(actual));
    }

    /// `serde_json` is built with `preserve_order`, so `HashMap`-backed
    /// manifest maps serialize in nondeterministic order; sort recursively
    /// for a stable snapshot.
    fn sorted_json(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|(left, _), (right, _)| left.cmp(right));
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, sorted_json(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sorted_json).collect())
            }
            other => other,
        }
    }

    #[test]
    fn build_manifest_keeps_legacy_parent_paths_for_external_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("checkout");
        let root = temp.path().join("user/workflows/root");
        let child = temp.path().join("user/workflows/child");
        std::fs::create_dir_all(&cwd).unwrap();
        for directory in [&root, &child] {
            std::fs::create_dir_all(directory.join("prompts")).unwrap();
            std::fs::write(
                directory.join("workflow.toml"),
                "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
            )
            .unwrap();
        }
        std::fs::write(
            root.join("workflow.fabro"),
            r#"digraph Root {
                start [shape=Mdiamond]
                prompt [prompt="@prompts/root.md"]
                child [shape=house, stack.child_workflow="../child/workflow.fabro"]
                exit [shape=Msquare]
                start -> prompt -> child -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(root.join("prompts/root.md"), "root prompt\n").unwrap();
        std::fs::write(
            child.join("workflow.fabro"),
            r#"digraph Child {
                start [shape=Mdiamond]
                prompt [prompt="@prompts/child.md"]
                exit [shape=Msquare]
                start -> prompt -> exit
            }"#,
        )
        .unwrap();
        std::fs::write(child.join("prompts/child.md"), "child prompt\n").unwrap();

        let built = build_run_manifest(ManifestBuildInput {
            workflow: root.join("workflow.fabro"),
            cwd,
            environment_defaults: test_environment_defaults(),
            ..ManifestBuildInput::default()
        })
        .unwrap();

        let root_key = "../user/workflows/root/workflow.fabro";
        let child_key = "../user/workflows/child/workflow.fabro";
        assert_eq!(built.manifest.target.path, root_key);
        let root_workflow = &built.manifest.workflows[root_key];
        assert_eq!(
            root_workflow.config.as_ref().unwrap().path,
            "../user/workflows/root/workflow.toml"
        );
        let root_prompt = &root_workflow.files["../user/workflows/root/prompts/root.md"];
        assert_eq!(root_prompt.ref_.from.as_deref(), Some(root_key));
        assert_eq!(root_prompt.ref_.original, "prompts/root.md");
        let child_workflow = &built.manifest.workflows[child_key];
        assert_eq!(
            child_workflow.config.as_ref().unwrap().path,
            "../user/workflows/child/workflow.toml"
        );
        let child_prompt = &child_workflow.files["../user/workflows/child/prompts/child.md"];
        assert_eq!(child_prompt.ref_.from.as_deref(), Some(child_key));
    }

    #[test]
    fn build_run_overrides_sets_common_cli_and_mcp_layers() {
        let overrides = build_run_overrides(RunOverrideInput {
            goal:             Some("ship it"),
            model:            Some("gpt-5.4-mini"),
            provider:         Some("openai"),
            environment:      Some("local"),
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
                .as_str(),
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
                .as_str(),
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
    fn sparse_run_overrides_preserve_only_has_no_image() {
        let overrides = build_sparse_run_overrides(RunOverrideInput {
            preserve_sandbox: Some(true),
            ..RunOverrideInput::default()
        })
        .expect("preserve override");
        let environment = overrides.environment.expect("environment override");

        assert!(environment.image.is_none());
        assert_eq!(
            environment.lifecycle.expect("lifecycle override").preserve,
            Some(true)
        );
    }

    #[test]
    fn sparse_run_overrides_environment_only_has_no_image() {
        let overrides = build_sparse_run_overrides(RunOverrideInput {
            environment: Some("local"),
            ..RunOverrideInput::default()
        })
        .expect("environment override");
        let environment = overrides.environment.expect("environment override");

        assert_eq!(environment.id.as_deref(), Some("local"));
        assert!(environment.image.is_none());
    }

    #[test]
    fn sparse_run_overrides_default_is_empty() {
        assert!(build_sparse_run_overrides(RunOverrideInput::default()).is_none());
    }

    // Regression coverage for https://github.com/fabro-sh/fabro/issues/476.
    #[test]
    fn build_manifest_bundles_agent_output_schema_file() {
        assert_manifest_bundles_output_schema_file(r#"type="agent", prompt="Return JSON""#);
    }

    #[test]
    fn build_manifest_bundles_command_output_schema_file() {
        assert_manifest_bundles_output_schema_file(r#"type="command", script="echo""#);
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
            serde_json::to_value(&built.manifest.target).unwrap(),
            serde_json::json!({ "path": ".fabro/workflows/demo/workflow.fabro" })
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
        assert_eq!(
            serde_json::to_value(built.manifest.goal.as_ref().unwrap()).unwrap(),
            serde_json::json!({ "type": "graph", "text": "ship it" })
        );
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
        assert_eq!(
            root.files[".fabro/workflows/demo/prompts/goal.tpl.md"]
                .ref_
                .from
                .as_deref(),
            Some(".fabro/workflows/demo/prompts/goal.md")
        );
        assert_eq!(
            root.files[".fabro/workflows/demo/prompts/plan.tpl.md"]
                .ref_
                .from
                .as_deref(),
            Some(".fabro/workflows/demo/prompts/plan.md")
        );
        assert_eq!(
            root.files[".fabro/workflows/demo/inline.tpl.md"]
                .ref_
                .from
                .as_deref(),
            Some(".fabro/workflows/demo/workflow.fabro")
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
            err.chain().any(|cause| cause
                .downcast_ref::<fabro_template::TemplateDiscoveryError>()
                .is_some()),
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
        assert_eq!(
            serde_json::to_value(&goal).unwrap(),
            serde_json::json!({ "type": "file", "text": "ship from project root" })
        );
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
        assert_eq!(
            serde_json::to_value(&goal).unwrap(),
            serde_json::json!({ "type": "file", "text": "ship from workflow dir" })
        );
    }

    /// The wire-level goal carries only the resolved kind and content:
    /// inline and file-sourced goals serialize to exactly `type` + `text`.
    #[test]
    fn resolved_goals_serialize_type_and_text_only() {
        let inline = resolved_goal_to_manifest(ResolvedRunGoal {
            text:   "inline goal".to_string(),
            source: ResolvedGoalSource::Inline,
        });
        assert_eq!(
            serde_json::to_value(&inline).unwrap(),
            serde_json::json!({ "type": "value", "text": "inline goal" })
        );

        let file = resolved_goal_to_manifest(ResolvedRunGoal {
            text:   "goal from file".to_string(),
            source: ResolvedGoalSource::File {
                path: PathBuf::from("/tmp/project/goal.md"),
            },
        });
        assert_eq!(
            serde_json::to_value(&file).unwrap(),
            serde_json::json!({ "type": "file", "text": "goal from file" })
        );
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
    }

    /// A local branch ahead of its origin is pushed as a side effect of
    /// building the manifest, so clone-based execution sees local commits.
    #[test]
    fn build_manifest_pushes_local_commits_to_bare_origin() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bare_origin = init_bare_origin(temp.path());

        init_git_repo(&workspace, "feature", bare_origin.to_str().unwrap());

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

        let built = build_run_manifest(ManifestBuildInput {
            workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
            cwd: workspace.clone(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap();

        assert!(built.manifest.git.is_some());
        let local_head = head_sha(&workspace).expect("workspace HEAD should resolve");
        assert_eq!(
            bare_remote_branch_sha(&bare_origin, "feature").as_deref(),
            Some(local_head.trim()),
            "the local branch should be pushed to the bare origin during manifest build",
        );
    }

    #[test]
    fn build_manifest_git_skips_push_when_configured_repository_differs_from_origin() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bare_origin = init_bare_origin(temp.path());

        init_git_repo(&workspace, "feature", bare_origin.to_str().unwrap());

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
            cwd: workspace.clone(),
            environment_defaults: test_environment_defaults(),
            ..Default::default()
        })
        .unwrap();

        let git = built
            .manifest
            .git
            .expect("manifest git info should be detected");
        assert_eq!(git.origin_url, "https://github.com/example/target");
        assert_eq!(
            bare_remote_branch_sha(&bare_origin, "feature"),
            None,
            "a mismatched configured repository must not be pushed to",
        );
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
                .expect("a failed push must not fail manifest creation");

                assert!(built.manifest.git.is_some());
            });
        });

        assert_eq!(std::fs::read_to_string(helper_log).unwrap(), "0\n");
    }

    #[test]
    fn observes_synced_github_branch_as_an_exact_target() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bare_origin = init_bare_origin(temp.path());
        init_git_repo(&workspace, "feature", "https://github.com/acme/widgets.git");
        let local_url = format!("file://{}", bare_origin.display());
        run_git(&workspace, &[
            "config",
            &format!("url.{local_url}.insteadOf"),
            "https://github.com/acme/widgets.git",
        ]);
        run_git(&workspace, &["push", "origin", "feature"]);

        let observation =
            observe_git_run_target(&workspace, Some("https://github.com/acme/widgets")).unwrap();
        let target = observation.run_target.as_ref().unwrap();
        let legacy = &observation.legacy_git_context;

        assert_eq!(target.repo, "acme/widgets");
        assert_eq!(target.branch, "feature");
        assert_eq!(target.tag, None);
        assert_eq!(target.sha, legacy.sha);
        assert_eq!(legacy.dirty, DirtyStatus::Clean);
        assert_eq!(legacy.origin_url, "https://github.com/acme/widgets");
    }

    #[test]
    fn observes_exact_sha_only_after_a_successful_noninteractive_push() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bare_origin = init_bare_origin(temp.path());
        init_git_repo(&workspace, "feature", "https://github.com/acme/widgets");
        let local_url = format!("file://{}", bare_origin.display());
        run_git(&workspace, &[
            "config",
            &format!("url.{local_url}.insteadOf"),
            "https://github.com/acme/widgets",
        ]);

        let observation =
            observe_git_run_target(&workspace, Some("https://github.com/acme/widgets")).unwrap();
        let target = observation.run_target.as_ref().unwrap();

        assert_eq!(target.sha, observation.legacy_git_context.sha);
        assert_eq!(bare_remote_branch_sha(&bare_origin, "feature"), target.sha);
    }

    #[test]
    fn stale_matching_tracking_ref_produces_a_branch_only_target() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bare_origin = init_bare_origin(temp.path());
        init_git_repo(&workspace, "feature", "https://github.com/acme/widgets");
        let local_url = format!("file://{}", bare_origin.display());
        run_git(&workspace, &[
            "config",
            &format!("url.{local_url}.insteadOf"),
            "https://github.com/acme/widgets",
        ]);
        run_git(&workspace, &["push", "origin", "feature"]);
        run_git(&workspace, &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "local-only",
        ]);
        mark_origin_branch_synced(&workspace, "feature");

        let observation =
            observe_git_run_target(&workspace, Some("https://github.com/acme/widgets")).unwrap();
        let target = observation.run_target.as_ref().unwrap();

        assert_eq!(target.sha, None);
        assert!(observation.legacy_git_context.sha.is_some());
        assert_ne!(
            bare_remote_branch_sha(&bare_origin, "feature"),
            observation.legacy_git_context.sha,
        );
    }

    #[test]
    fn branches_that_are_invalid_run_selectors_do_not_produce_git_targets() {
        let temp = tempfile::tempdir().unwrap();
        let invalid_branches = [
            "heads/topic",
            "tags/release",
            "0123456789abcdef0123456789abcdef01234567",
        ];

        for (index, branch) in invalid_branches.into_iter().enumerate() {
            let workspace = temp.path().join(format!("workspace-{index}"));
            std::fs::create_dir_all(&workspace).unwrap();
            init_git_repo(&workspace, branch, "https://github.com/acme/widgets");

            let observation = observe_git_run_target(&workspace, None).unwrap();

            assert_eq!(observation.run_target, None, "branch {branch}");
            assert_eq!(observation.legacy_git_context.branch, branch);
        }
    }

    #[test]
    fn failed_push_and_origin_mismatch_produce_branch_only_targets() {
        let temp = tempfile::tempdir().unwrap();
        let failed_workspace = temp.path().join("failed");
        std::fs::create_dir_all(&failed_workspace).unwrap();
        init_git_repo(
            &failed_workspace,
            "feature",
            "https://github.com/acme/widgets",
        );
        let missing_url = format!("file://{}/missing.git", temp.path().display());
        run_git(&failed_workspace, &[
            "config",
            &format!("url.{missing_url}.insteadOf"),
            "https://github.com/acme/widgets",
        ]);

        let failed =
            observe_git_run_target(&failed_workspace, Some("https://github.com/acme/widgets"))
                .unwrap();
        assert_eq!(failed.run_target.as_ref().unwrap().sha, None);
        assert!(failed.legacy_git_context.sha.is_some());
        assert_eq!(failed.legacy_git_context.dirty, DirtyStatus::Clean);

        let mismatched_workspace = temp.path().join("mismatched");
        std::fs::create_dir_all(&mismatched_workspace).unwrap();
        let bare_origin = init_bare_origin(&temp.path().join("other"));
        init_git_repo(
            &mismatched_workspace,
            "feature",
            bare_origin.to_str().unwrap(),
        );

        let mismatched = observe_git_run_target(
            &mismatched_workspace,
            Some("https://github.com/acme/configured"),
        )
        .unwrap();
        let target = mismatched.run_target.as_ref().unwrap();
        assert_eq!(target.repo, "acme/configured");
        assert_eq!(target.sha, None);
        assert_eq!(bare_remote_branch_sha(&bare_origin, "feature"), None);
    }

    #[test]
    fn git_observation_distinguishes_dirty_unsupported_and_unusable_checkouts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let bare_origin = init_bare_origin(temp.path());
        init_git_repo(&workspace, "feature", bare_origin.to_str().unwrap());
        std::fs::write(workspace.join("dirty.txt"), "dirty").unwrap();

        let unsupported = observe_git_run_target(&workspace, None).unwrap();
        assert!(unsupported.run_target.is_none());
        assert_eq!(unsupported.legacy_git_context.dirty, DirtyStatus::Dirty);
        assert_eq!(
            unsupported.legacy_git_context.origin_url,
            fabro_github::normalize_repo_origin_url(&bare_origin.to_string_lossy()),
        );

        let not_repo = temp.path().join("not-repo");
        std::fs::create_dir_all(&not_repo).unwrap();
        assert!(observe_git_run_target(&not_repo, None).is_none());

        let unborn = temp.path().join("unborn");
        std::fs::create_dir_all(&unborn).unwrap();
        run_git(&unborn, &["init", "--quiet"]);
        assert!(observe_git_run_target(&unborn, None).is_none());

        run_git(&workspace, &["checkout", "--detach", "--quiet"]);
        assert!(observe_git_run_target(&workspace, None).is_none());
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

    fn init_bare_origin(parent: &Path) -> PathBuf {
        let bare = parent.join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        run_git(&bare, &["init", "--bare", "--quiet"]);
        bare
    }

    fn bare_remote_branch_sha(bare_path: &Path, branch: &str) -> Option<String> {
        let repo = git2::Repository::open_bare(bare_path).expect("bare origin should open");
        repo.find_reference(&format!("refs/heads/{branch}"))
            .ok()
            .and_then(|reference| reference.target())
            .map(|oid| oid.to_string())
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
