use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use fabro_config::parse::SettingsSource;
use fabro_config::{EnvironmentLayer, RunEnvironmentLayer, RunGoalLayer, SettingsLayer};
use fabro_environment::{EnvironmentId, EnvironmentValidationError};
use fabro_manifest::CollectedWorkflowClosure;
use fabro_types::settings::InterpString;
use fabro_types::{
    GitContext, ManifestPath, RunId, RunTarget, SandboxProviderKind, TargetValidationError,
    WorkflowPath, WorkflowVersion, WorkflowVersionId,
};
use fabro_workflow::git;
use fabro_workflow::workflow_bundle::{BundledWorkflow, ParsedWorkflowConfig, WorkflowBundle};
use fabro_workflow_version::{LoadedWorkflowVersionClosure, ValidatedWorkflowVersion};
use thiserror::Error;
use tokio::{fs, task};

use crate::run_compiler::{RunCompilerError, settings_layer_with_resolved_dockerfiles};

#[derive(Debug, Error)]
pub(crate) enum RunIntentAdmissionError {
    #[error("workflow-version closure could not be loaded")]
    VersionStore {
        #[source]
        source: fabro_workflow_version::WorkflowVersionStoreError,
    },
    #[error(transparent)]
    Lowering(#[from] WorkflowClosureLoweringError),
    #[error(transparent)]
    Target(#[from] TargetValidationError),
    #[error(transparent)]
    FolderTarget(#[from] FolderTargetValidationError),
    #[error(transparent)]
    Environment(#[from] EnvironmentSelectionError),
    #[error(transparent)]
    Compiler(#[from] RunCompilerError),
    #[error("run variables could not be loaded")]
    VariableSnapshot {
        #[source]
        source: fabro_variable::Error,
    },
    #[error("originating worker run `{run_id}` could not be loaded")]
    WorkerRun {
        run_id: RunId,
        #[source]
        source: fabro_store::Error,
    },
}

#[derive(Debug, Error)]
pub(crate) enum FolderTargetValidationError {
    #[error("folder target path must be absolute")]
    Relative,
    #[error("folder target path does not name an accessible filesystem entry")]
    Canonicalize {
        #[source]
        source: std::io::Error,
    },
    #[error("folder target path must name a directory")]
    NotDirectory,
    #[error("folder target canonical path must be valid UTF-8")]
    NonUtf8,
}

#[derive(Debug)]
pub(crate) struct PreparedIntentTarget {
    pub(crate) target: RunTarget,
    pub(crate) git:    Option<GitContext>,
}

/// Materialize filesystem-backed target facts after the effective environment
/// has been admitted as Local and before run allocation. Folder targets are
/// canonicalized once for durable identity and their optional Git metadata is
/// observed under the same provider gate, so rejected requests never scan host
/// repositories. Other targets pass through with their validated projection.
pub(crate) async fn prepare_intent_target(
    target: RunTarget,
    git: Option<GitContext>,
) -> Result<PreparedIntentTarget, FolderTargetValidationError> {
    let RunTarget::Folder { path } = target else {
        return Ok(PreparedIntentTarget { target, git });
    };
    let submitted = PathBuf::from(path);
    if !submitted.is_absolute() {
        return Err(FolderTargetValidationError::Relative);
    }
    let canonical = fs::canonicalize(&submitted)
        .await
        .map_err(|source| FolderTargetValidationError::Canonicalize { source })?;
    let metadata = fs::metadata(&canonical)
        .await
        .map_err(|source| FolderTargetValidationError::Canonicalize { source })?;
    if !metadata.is_dir() {
        return Err(FolderTargetValidationError::NotDirectory);
    }
    let path = canonical_folder_text(&canonical)?;
    let git = task::spawn_blocking(move || {
        git::observe_git_context(&canonical).unwrap_or_else(|error| {
            tracing::warn!(
                error = ?error,
                path = %canonical.display(),
                "failed to observe optional git metadata for folder target"
            );
            None
        })
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(
            error = ?error,
            path,
            "folder target git observation task failed"
        );
        None
    });

    Ok(PreparedIntentTarget {
        target: RunTarget::Folder { path },
        git,
    })
}

fn canonical_folder_text(path: &Path) -> Result<String, FolderTargetValidationError> {
    path.to_str()
        .map(str::to_string)
        .ok_or(FolderTargetValidationError::NonUtf8)
}

#[derive(Debug, Error)]
pub(crate) enum EnvironmentSelectionError {
    #[error("invalid environment ID `{value}`")]
    InvalidId {
        value:  String,
        #[source]
        source: EnvironmentValidationError,
    },
    #[error("environment `{id}` not found")]
    NotFound { id: EnvironmentId },
    #[error("{detail}")]
    TargetUnsupported { detail: &'static str },
    #[error(
        "automatic pull requests require a clone-based Docker or Daytona environment; disable run.pull_request.enabled for Local execution"
    )]
    AutomaticPullRequestUnsupported,
    #[error("{detail}")]
    ProviderDisabled {
        provider: SandboxProviderKind,
        detail:   String,
    },
    #[error("{name} is not configured for sandbox provider `{provider}`")]
    MissingCredential {
        provider: SandboxProviderKind,
        name:     &'static str,
    },
    #[error("failed to read sandbox credential `{name}`")]
    CredentialStore {
        name:   &'static str,
        #[source]
        source: fabro_vault::SecretStoreError,
    },
}

#[derive(Debug)]
pub(crate) struct LoweredWorkflowClosure {
    pub(crate) workflow_bundle: WorkflowBundle,
    pub(crate) entrypoint:      ManifestPath,
    pub(crate) workflow_layer:  Option<SettingsLayer>,
}

/// Ceiling on the number of distinct workflow mounts one closure may expand
/// into. Mounts are keyed by rebased path, so a small dependency graph that
/// re-mounts shared versions along many paths can otherwise expand
/// exponentially and stall admission on a single request.
const MAX_WORKFLOW_MOUNTS: usize = 256;

#[derive(Debug, Error)]
pub(crate) enum WorkflowClosureLoweringError {
    #[error("workflow version `{id}` is missing from the loaded closure")]
    MissingVersion { id: WorkflowVersionId },
    #[error("workflow path `{path}` cannot be mounted at `{mount}`")]
    InvalidMount {
        path:  WorkflowPath,
        mount: ManifestPath,
    },
    #[error("workflow mount `{path}` resolves to two different workflow versions")]
    ConflictingMount { path: ManifestPath },
    #[error("workflow version closure expands into more than {limit} workflow mounts")]
    MountLimitExceeded { limit: usize },
    #[error("workflow-version settings are unusable")]
    Settings {
        #[source]
        source: Box<RunCompilerError>,
    },
}

/// Read access to a workflow-version closure, whether loaded from the store
/// or collected from a local checkout, so both lower through one path.
trait WorkflowClosureView {
    fn root_id(&self) -> WorkflowVersionId;
    fn validated_root(&self) -> &ValidatedWorkflowVersion;
    fn get(&self, id: &WorkflowVersionId) -> Option<&WorkflowVersion>;

    fn root(&self) -> &WorkflowVersion {
        self.validated_root().version()
    }
}

impl WorkflowClosureView for LoadedWorkflowVersionClosure {
    fn root_id(&self) -> WorkflowVersionId {
        self.root_id()
    }

    fn validated_root(&self) -> &ValidatedWorkflowVersion {
        self.validated_root()
    }

    fn get(&self, id: &WorkflowVersionId) -> Option<&WorkflowVersion> {
        self.get(id)
    }
}

struct CollectedWorkflowClosureView<'a> {
    root_id:  WorkflowVersionId,
    root:     &'a ValidatedWorkflowVersion,
    versions: HashMap<WorkflowVersionId, &'a ValidatedWorkflowVersion>,
}

impl<'a> CollectedWorkflowClosureView<'a> {
    fn new(closure: &'a CollectedWorkflowClosure) -> Result<Self, WorkflowClosureLoweringError> {
        let root_id = closure.root_id();
        let versions = closure.versions().collect::<HashMap<_, _>>();
        let root = *versions
            .get(&root_id)
            .ok_or(WorkflowClosureLoweringError::MissingVersion { id: root_id })?;
        Ok(Self {
            root_id,
            root,
            versions,
        })
    }
}

impl WorkflowClosureView for CollectedWorkflowClosureView<'_> {
    fn root_id(&self) -> WorkflowVersionId {
        self.root_id
    }

    fn validated_root(&self) -> &ValidatedWorkflowVersion {
        self.root
    }

    fn get(&self, id: &WorkflowVersionId) -> Option<&WorkflowVersion> {
        self.versions.get(id).map(|version| version.version())
    }
}

fn lower_workflow_closure_view(
    closure: &impl WorkflowClosureView,
) -> Result<LoweredWorkflowClosure, WorkflowClosureLoweringError> {
    let entrypoint = manifest_path(closure.root().entrypoint(), closure.root().entrypoint())?;
    let mut mounts = HashMap::new();
    let mut workflows = HashMap::new();
    mount_version(
        closure,
        closure.root_id(),
        entrypoint.clone(),
        &mut mounts,
        &mut workflows,
    )?;

    let root_workflow = workflows
        .get(&entrypoint)
        .expect("root workflow should be mounted");
    let workflow_layer = root_workflow
        .config
        .as_ref()
        .map(|config| {
            settings_layer_with_resolved_dockerfiles(
                &config.source,
                &config.path,
                &root_workflow.files,
                SettingsSource::Workflow,
            )
            .map_err(|source| WorkflowClosureLoweringError::Settings {
                source: Box::new(source),
            })
            .map(|mut layer| {
                inline_goal_file(&mut layer, closure.validated_root());
                layer
            })
        })
        .transpose()?;

    Ok(LoweredWorkflowClosure {
        workflow_bundle: WorkflowBundle::new(workflows),
        entrypoint,
        workflow_layer,
    })
}

pub(crate) fn lower_workflow_closure(
    closure: &LoadedWorkflowVersionClosure,
) -> Result<LoweredWorkflowClosure, WorkflowClosureLoweringError> {
    lower_workflow_closure_view(closure)
}

pub(crate) fn lower_collected_workflow_closure(
    closure: &CollectedWorkflowClosure,
) -> Result<LoweredWorkflowClosure, WorkflowClosureLoweringError> {
    let view = CollectedWorkflowClosureView::new(closure)?;
    lower_workflow_closure_view(&view)
}

pub(crate) fn pin_workflow_environment_authority(layer: &mut SettingsLayer, environment_id: &str) {
    // Both blocks destructure without `..` so adding a field to either layer
    // type forces a compile-time decision here: server-owned facts are
    // cleared off the immutable workflow layer, workflow-owned facts pass
    // through.
    if let Some(environment) = layer.environments.get_mut(environment_id) {
        let EnvironmentLayer {
            provider,
            cwd,
            image,
            resources: _,
            network: _,
            lifecycle: _,
            labels: _,
            env: _,
        } = environment;
        *provider = None;
        *cwd = None;
        *image = None;
    }
    if let Some(environment) = layer.run.as_mut().and_then(|run| run.environment.as_mut()) {
        let RunEnvironmentLayer {
            id: _,
            image,
            resources: _,
            network: _,
            lifecycle: _,
            labels: _,
            env: _,
        } = environment;
        *image = None;
    }
}

fn mount_version(
    closure: &impl WorkflowClosureView,
    id: WorkflowVersionId,
    mounted_entrypoint: ManifestPath,
    mounts: &mut HashMap<ManifestPath, WorkflowVersionId>,
    workflows: &mut HashMap<ManifestPath, BundledWorkflow>,
) -> Result<(), WorkflowClosureLoweringError> {
    if let Some(existing) = mounts.get(&mounted_entrypoint) {
        return if *existing == id {
            Ok(())
        } else {
            Err(WorkflowClosureLoweringError::ConflictingMount {
                path: mounted_entrypoint,
            })
        };
    }
    mounts.insert(mounted_entrypoint.clone(), id);
    // Recursion depth is bounded by the mount count (every level inserts a
    // distinct mount before descending), so this cap also bounds the stack.
    if mounts.len() > MAX_WORKFLOW_MOUNTS {
        return Err(WorkflowClosureLoweringError::MountLimitExceeded {
            limit: MAX_WORKFLOW_MOUNTS,
        });
    }

    let version = closure
        .get(&id)
        .ok_or(WorkflowClosureLoweringError::MissingVersion { id })?;
    let mut files = HashMap::new();
    for (path, content) in version.files() {
        files.insert(
            manifest_path(version.entrypoint(), path).and_then(|local| {
                rebase_path(version.entrypoint(), &mounted_entrypoint, &local, path)
            })?,
            content.clone(),
        );
    }
    let source = version
        .files()
        .get(version.entrypoint())
        .cloned()
        .expect("validated workflow versions contain their entrypoint file");
    let config_local = version.config_path();
    let config_path = version.files().get(&config_local).map(|source| {
        rebase_path(
            version.entrypoint(),
            &mounted_entrypoint,
            &ManifestPath::from_wire(config_local.as_str())
                .expect("validated workflow path should be a manifest path"),
            &config_local,
        )
        .map(|path| ParsedWorkflowConfig {
            path,
            source: source.clone(),
        })
    });
    let config = config_path.transpose()?;

    workflows.insert(mounted_entrypoint.clone(), BundledWorkflow {
        path: mounted_entrypoint.clone(),
        source,
        config,
        files,
    });

    for (binding, dependency_id) in version.workflow_dependencies() {
        let local = ManifestPath::from_wire(binding.as_str())
            .expect("validated workflow path should be a manifest path");
        let dependency_mount =
            rebase_path(version.entrypoint(), &mounted_entrypoint, &local, binding)?;
        mount_version(closure, *dependency_id, dependency_mount, mounts, workflows)?;
    }
    Ok(())
}

fn manifest_path(
    entrypoint: &WorkflowPath,
    path: &WorkflowPath,
) -> Result<ManifestPath, WorkflowClosureLoweringError> {
    ManifestPath::from_wire(path.as_str()).ok_or_else(|| {
        WorkflowClosureLoweringError::InvalidMount {
            path:  path.clone(),
            mount: ManifestPath::from_wire(entrypoint.as_str())
                .expect("validated entrypoint should be a manifest path"),
        }
    })
}

fn rebase_path(
    local_entrypoint: &WorkflowPath,
    mounted_entrypoint: &ManifestPath,
    local_path: &ManifestPath,
    workflow_path: &WorkflowPath,
) -> Result<ManifestPath, WorkflowClosureLoweringError> {
    let relative = relative_path(
        local_entrypoint_parent(local_entrypoint),
        local_path.as_path(),
    );
    let mapped = ManifestPath::from_reference(
        mounted_entrypoint.parent_or_dot(),
        &relative.to_string_lossy(),
    )
    .filter(|path| !path.as_path().starts_with(".."))
    .ok_or_else(|| WorkflowClosureLoweringError::InvalidMount {
        path:  workflow_path.clone(),
        mount: mounted_entrypoint.clone(),
    })?;
    Ok(mapped)
}

fn local_entrypoint_parent(entrypoint: &WorkflowPath) -> &Path {
    Path::new(entrypoint.as_str())
        .parent()
        .unwrap_or_else(|| Path::new("."))
}

fn relative_path(base: &Path, path: &Path) -> PathBuf {
    let base = base
        .components()
        .filter_map(normal_component)
        .collect::<Vec<_>>();
    let path = path
        .components()
        .filter_map(normal_component)
        .collect::<Vec<_>>();
    let common = base
        .iter()
        .zip(&path)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in &base[common..] {
        relative.push("..");
    }
    for component in &path[common..] {
        relative.push(component);
    }
    relative
}

fn normal_component(component: Component<'_>) -> Option<&std::ffi::OsStr> {
    match component {
        Component::Normal(value) => Some(value),
        Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
            None
        }
    }
}

/// Inline a file-form run goal using the goal-file resolution the
/// workflow-version store certified, so the resolution grammar has exactly
/// one owner in `fabro-workflow-version`.
fn inline_goal_file(layer: &mut SettingsLayer, root: &ValidatedWorkflowVersion) {
    let Some(goal) = layer.run.as_mut().and_then(|run| run.goal.as_mut()) else {
        return;
    };
    if !matches!(&*goal, RunGoalLayer::File { .. }) {
        return;
    }
    // `layer` is parsed from the same `workflow.toml` source the certified
    // root version carries, so a file-form goal here always resolves there.
    let content = root
        .resolved_goal_file_content()
        .expect("stored workflow versions certify their goal-file references");
    *goal = RunGoalLayer::Inline(InterpString::parse(content));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fabro_types::{WorkflowVersion, WorkflowVersionId};
    use fabro_workflow_version::{ValidatedWorkflowVersion, WorkflowVersionStore};

    use super::*;

    fn workflow_path(value: &str) -> WorkflowPath {
        WorkflowPath::new(value).unwrap()
    }

    fn version(
        entrypoint: &str,
        files: impl IntoIterator<Item = (&'static str, &'static str)>,
        dependencies: impl IntoIterator<Item = (&'static str, WorkflowVersionId)>,
    ) -> ValidatedWorkflowVersion {
        let files = files
            .into_iter()
            .map(|(path, source)| (workflow_path(path), source.to_string()))
            .collect();
        let dependencies = dependencies
            .into_iter()
            .map(|(path, id)| (workflow_path(path), id))
            .collect::<BTreeMap<_, _>>();
        ValidatedWorkflowVersion::new(
            WorkflowVersion::new(workflow_path(entrypoint), files, dependencies).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn collected_and_stored_closures_lower_through_the_same_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let child = temp.path().join("child");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        fs::write(
            root.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n[run.goal]\nfile = \"goal.md\"\n",
        )
        .await
        .unwrap();
        fs::write(
            root.join("workflow.fabro"),
            "digraph Root { child [stack.child_workflow=\"../child/workflow.fabro\"] }",
        )
        .await
        .unwrap();
        fs::write(root.join("goal.md"), "Ship it").await.unwrap();
        fs::write(child.join("workflow.fabro"), "digraph Child {}")
            .await
            .unwrap();

        let collected =
            fabro_manifest::collect_workflow_versions(&root.join("workflow.toml"), temp.path())
                .unwrap();
        let (database, _) = crate::test_support::test_store_bundle();
        let store = WorkflowVersionStore::new(database.blobs());
        for (_, version) in collected.versions() {
            store.put(version).await.unwrap();
        }
        let stored = store
            .get_closure(&collected.root_id())
            .await
            .unwrap()
            .unwrap();

        let from_collected = lower_collected_workflow_closure(&collected).unwrap();
        let from_stored = lower_workflow_closure(&stored).unwrap();

        assert_eq!(from_collected.entrypoint, from_stored.entrypoint);
        assert_eq!(
            serde_json::to_value(from_collected.workflow_bundle.workflows()).unwrap(),
            serde_json::to_value(from_stored.workflow_bundle.workflows()).unwrap(),
        );
        assert_eq!(
            format!("{:?}", from_collected.workflow_layer),
            format!("{:?}", from_stored.workflow_layer),
        );
    }

    #[tokio::test]
    async fn prepares_a_canonical_folder_target_without_git_projection() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target");
        std::fs::create_dir(&target_dir).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        let submitted = dir.path().join("nested").join("..").join("target");

        let prepared = prepare_intent_target(
            RunTarget::Folder {
                path: submitted.to_string_lossy().to_string(),
            },
            None,
        )
        .await
        .unwrap();
        let canonical = target_dir.canonicalize().unwrap();

        assert_eq!(prepared.git, None);
        assert_eq!(prepared.target, RunTarget::Folder {
            path: canonical.to_string_lossy().to_string(),
        });
    }

    #[tokio::test]
    async fn rejects_relative_missing_and_file_folder_targets() {
        let relative = prepare_intent_target(
            RunTarget::Folder {
                path: "relative/path".to_string(),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(relative, FolderTargetValidationError::Relative));

        let dir = tempfile::tempdir().unwrap();
        let missing = prepare_intent_target(
            RunTarget::Folder {
                path: dir.path().join("missing").to_string_lossy().to_string(),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            missing,
            FolderTargetValidationError::Canonicalize { .. }
        ));

        let file = dir.path().join("file");
        fs::write(&file, "not a directory").await.unwrap();
        let file = prepare_intent_target(
            RunTarget::Folder {
                path: file.to_string_lossy().to_string(),
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(file, FolderTargetValidationError::NotDirectory));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_non_utf8_canonical_folder_target() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(OsString::from_vec(vec![b'f', b'o', 0x80]));
        let error = canonical_folder_text(&path).unwrap_err();

        assert!(matches!(error, FolderTargetValidationError::NonUtf8));
    }

    #[tokio::test]
    async fn lowers_nested_entrypoints_and_inlines_goal_files() {
        let (database, _) = crate::test_support::test_store_bundle();
        let blobs = database.blobs();
        let store = WorkflowVersionStore::new(blobs);
        let grandchild = version(
            "deep/leaf.fabro",
            [("deep/leaf.fabro", "digraph Leaf {}")],
            [],
        );
        let grandchild_id = store.put(&grandchild).await.unwrap();
        let child = version(
            "pkg/child.fabro",
            [(
                "pkg/child.fabro",
                "digraph Child { leaf [stack.child_workflow=\"../nested/leaf.fabro\"] }",
            )],
            [("nested/leaf.fabro", grandchild_id)],
        );
        let child_id = store.put(&child).await.unwrap();
        let root = version(
            "flows/root.fabro",
            [
                (
                    "flows/root.fabro",
                    "digraph Root { child [stack.child_workflow=\"../deps/run.fabro\"] }",
                ),
                (
                    "flows/workflow.toml",
                    "_version = 1\n[run.goal]\nfile = \"goal.md\"\n",
                ),
                ("flows/goal.md", "Ship {{ vars.owner }}"),
            ],
            [("deps/run.fabro", child_id)],
        );
        let root_id = store.put(&root).await.unwrap();
        let closure = store.get_closure(&root_id).await.unwrap().unwrap();

        let lowered = lower_workflow_closure(&closure).unwrap();

        assert!(
            lowered
                .workflow_bundle
                .workflow(&lowered.entrypoint)
                .is_some()
        );
        assert!(
            lowered
                .workflow_bundle
                .workflow(&ManifestPath::from_wire("deps/run.fabro").unwrap())
                .is_some()
        );
        assert!(
            lowered
                .workflow_bundle
                .workflow(&ManifestPath::from_wire("nested/leaf.fabro").unwrap())
                .is_some()
        );
        assert!(matches!(
            lowered
                .workflow_layer
                .as_ref()
                .and_then(|layer| layer.run.as_ref())
                .and_then(|run| run.goal.as_ref()),
            Some(RunGoalLayer::Inline(_))
        ));
    }

    #[tokio::test]
    async fn lowers_same_version_at_distinct_mount_paths() {
        let (database, _) = crate::test_support::test_store_bundle();
        let blobs = database.blobs();
        let store = WorkflowVersionStore::new(blobs);
        let child = version(
            "pkg/child.fabro",
            [("pkg/child.fabro", "digraph Child {}")],
            [],
        );
        let child_id = store.put(&child).await.unwrap();
        let root = version(
            "flows/root.fabro",
            [(
                "flows/root.fabro",
                "digraph Root { one [stack.child_workflow=\"../children/one.fabro\"] two [stack.child_workflow=\"../children/two.fabro\"] }",
            )],
            [
                ("children/one.fabro", child_id),
                ("children/two.fabro", child_id),
            ],
        );
        let root_id = store.put(&root).await.unwrap();
        let closure = store.get_closure(&root_id).await.unwrap().unwrap();

        let lowered = lower_workflow_closure(&closure).unwrap();

        assert!(
            lowered
                .workflow_bundle
                .workflow(&ManifestPath::from_wire("children/one.fabro").unwrap())
                .is_some()
        );
        assert!(
            lowered
                .workflow_bundle
                .workflow(&ManifestPath::from_wire("children/two.fabro").unwrap())
                .is_some()
        );
    }

    #[tokio::test]
    async fn rejects_closures_that_expand_past_the_mount_limit() {
        let (database, _) = crate::test_support::test_store_bundle();
        let blobs = database.blobs();
        let store = WorkflowVersionStore::new(blobs);
        // A chain of tiny versions where each level mounts the next twice is
        // cheap to store and load (the closure dedupes by id) but expands to
        // 2^depth distinct mounts.
        let leaf = version("flow.fabro", [("flow.fabro", "digraph Leaf {}")], []);
        let mut previous = store.put(&leaf).await.unwrap();
        for _ in 0..9 {
            let fan = version(
                "flow.fabro",
                [(
                    "flow.fabro",
                    "digraph Fan { a [stack.child_workflow=\"a/next.fabro\"] b [stack.child_workflow=\"b/next.fabro\"] }",
                )],
                [("a/next.fabro", previous), ("b/next.fabro", previous)],
            );
            previous = store.put(&fan).await.unwrap();
        }
        let closure = store.get_closure(&previous).await.unwrap().unwrap();

        let error = lower_workflow_closure(&closure).unwrap_err();

        assert!(matches!(
            error,
            WorkflowClosureLoweringError::MountLimitExceeded {
                limit: MAX_WORKFLOW_MOUNTS,
            }
        ));
    }

    #[tokio::test]
    async fn rejects_distinct_versions_that_converge_on_one_mount_path() {
        let (database, _) = crate::test_support::test_store_bundle();
        let blobs = database.blobs();
        let store = WorkflowVersionStore::new(blobs);
        let first_leaf = version(
            "leaf/first.fabro",
            [("leaf/first.fabro", "digraph FirstLeaf {}")],
            [],
        );
        let first_leaf_id = store.put(&first_leaf).await.unwrap();
        let second_leaf = version(
            "leaf/second.fabro",
            [("leaf/second.fabro", "digraph SecondLeaf {}")],
            [],
        );
        let second_leaf_id = store.put(&second_leaf).await.unwrap();
        let first_parent = version(
            "a/first.fabro",
            [(
                "a/first.fabro",
                "digraph FirstParent { child [stack.child_workflow=\"../shared/collision.fabro\"] }",
            )],
            [("shared/collision.fabro", first_leaf_id)],
        );
        let first_parent_id = store.put(&first_parent).await.unwrap();
        let second_parent = version(
            "b/second.fabro",
            [(
                "b/second.fabro",
                "digraph SecondParent { child [stack.child_workflow=\"../shared/collision.fabro\"] }",
            )],
            [("shared/collision.fabro", second_leaf_id)],
        );
        let second_parent_id = store.put(&second_parent).await.unwrap();
        let root = version(
            "flows/root.fabro",
            [(
                "flows/root.fabro",
                "digraph Root { first [stack.child_workflow=\"../left/first.fabro\"] second [stack.child_workflow=\"../right/second.fabro\"] }",
            )],
            [
                ("left/first.fabro", first_parent_id),
                ("right/second.fabro", second_parent_id),
            ],
        );
        let root_id = store.put(&root).await.unwrap();
        let closure = store.get_closure(&root_id).await.unwrap().unwrap();

        let error = lower_workflow_closure(&closure).unwrap_err();

        assert!(matches!(
            error,
            WorkflowClosureLoweringError::ConflictingMount { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_rebased_files_that_escape_the_runtime_root() {
        let (database, _) = crate::test_support::test_store_bundle();
        let blobs = database.blobs();
        let store = WorkflowVersionStore::new(blobs);
        let child = version(
            "nested/child.fabro",
            [
                ("nested/child.fabro", "digraph Child {}"),
                ("workflow.toml", "_version = 1"),
            ],
            [],
        );
        let child_id = store.put(&child).await.unwrap();
        let root = version(
            "root.fabro",
            [(
                "root.fabro",
                "digraph Root { child [stack.child_workflow=\"child.fabro\"] }",
            )],
            [("child.fabro", child_id)],
        );
        let root_id = store.put(&root).await.unwrap();
        let closure = store.get_closure(&root_id).await.unwrap().unwrap();

        let error = lower_workflow_closure(&closure).unwrap_err();

        assert!(matches!(
            error,
            WorkflowClosureLoweringError::InvalidMount { .. }
        ));
    }
}
