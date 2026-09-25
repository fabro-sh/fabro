//! Fabro's hooks on a Petri run, in process: command-only bundles run
//! through `engine::run` on the host sandbox with the memory store and
//! in-memory platform records, and the checkpoint commit, its record, the
//! failure route, the fatal checkpoint, and the run-end hooks are checked
//! against the workspace's Git history and the run's records.
//!
//! Every run acquires its scope through the sandbox-driver host plugin, so
//! the tests skip when that executable is not found, unless
//! `FABRO_REQUIRE_SANDBOX_PLUGINS` is set. The crash cases of the recovery
//! protocol need a worker to kill and live in the CLI's scenario suite.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests locate the plugin executable through the process environment and read the workspace's history with git"
)]
#![expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_checkpoint::author::GitAuthor;
use fabro_petri::admission::AdmittedGraphs;
use fabro_petri::artifacts::StoreArtifactWriter;
use fabro_petri::blobs::Blobs;
use fabro_petri::check::{self, Bundle, CheckRequest, Launch};
use fabro_petri::checkpoint::{
    CHECKPOINT_FAILED_CLASS, CheckpointKey, RunGitSettings, RunWorkspaces,
};
use fabro_petri::controls::RunControls;
use fabro_petri::engine::{self, Execution, RunRequest, RunStatus};
use fabro_petri::hooks::HooksSpec;
use fabro_petri::platform_records::PlatformRecords;
use fabro_petri::recovery::{self, Recovery, RecoveryRequest};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::test_support::{MemoryBlobs, MemoryPlatformRecords};
use fabro_store::{ArtifactStore, PlatformRecord, PlatformRecordKind};
use fabro_types::settings::run::RunCheckpointSettings;
use fabro_types::{GitIdentitySource, RunId, SandboxProviderKind};
use object_store::local::LocalFileSystem;
use petri_execution::inspect::{self, RunInspection};
use petri_store::{Access, MemoryRunStore, RunKey, RunStore as _};
use tokio::fs;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

mod support;

const HOST_PLUGIN: &str = "sandbox-driver-host";
const HOST_PLUGIN_OVERRIDE: &str = "PETRI_SANDBOX_HOST_PLUGIN";
const DOCKER_PLUGIN: &str = "sandbox-driver-docker";
const DOCKER_PLUGIN_OVERRIDE: &str = "PETRI_SANDBOX_DOCKER_PLUGIN";
const REQUIRE_ENV: &str = "FABRO_REQUIRE_SANDBOX_PLUGINS";

/// The host plugin as Petri's lookup finds it: the override variable, else
/// the executable on `PATH`. `None`, after saying so, when the test should
/// skip; a panic when the environment forbids a skip.
fn host_plugin() -> Option<PathBuf> {
    let found = env::var_os(HOST_PLUGIN_OVERRIDE)
        .map(PathBuf::from)
        .or_else(|| {
            env::split_paths(&env::var_os("PATH")?)
                .map(|dir| dir.join(HOST_PLUGIN))
                .find(|candidate| candidate.is_file())
        });
    if found.is_none() {
        assert!(
            env::var_os(REQUIRE_ENV).is_none(),
            "{REQUIRE_ENV} is set, but {HOST_PLUGIN} is not on PATH and {HOST_PLUGIN_OVERRIDE} is unset"
        );
        eprintln!("skipping: {HOST_PLUGIN} is not on PATH and {HOST_PLUGIN_OVERRIDE} is unset");
    }
    found
}

/// A command-only bundle: the stage lines go between `start` and `exit`,
/// the edge lines after them.
fn workflow(stages: &str, edges: &str) -> String {
    format!(
        "digraph Hooks {{\n  graph [goal=\"Check the hooks\", default_max_retries=0]\n  start \
         [shape=Mdiamond]\n  exit [shape=Msquare]\n{stages}\n{edges}\n}}\n"
    )
}

const SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

/// The bundle admitted the way the create handler admits it.
fn admit(workflow: &str, settings: &str) -> AdmittedGraphs {
    let request = CheckRequest {
        bundle:             Bundle {
            files:        BTreeMap::from([
                ("workflow.fabro".to_string(), workflow.to_string()),
                ("workflow.toml".to_string(), settings.to_string()),
            ]),
            entrypoint:   "workflow.fabro".to_string(),
            project_toml: None,
        },
        inputs:             BTreeMap::new(),
        vars:               BTreeMap::new(),
        launch:             Launch::default(),
        runtime:            RuntimeSpec::default(),
        unbound_is_warning: false,
    };
    let admitted = check::check(&request).expect("the bundle is admitted");
    AdmittedGraphs {
        graph:    admitted.graph,
        children: admitted.children,
    }
}

/// The Docker plugin as Petri's lookup finds it, with a daemon that
/// answers. `None`, after saying so, when the test should skip; a panic
/// when the environment forbids a skip and the plugin is missing.
fn docker_plugin() -> Option<PathBuf> {
    let found = env::var_os(DOCKER_PLUGIN_OVERRIDE)
        .map(PathBuf::from)
        .or_else(|| {
            env::split_paths(&env::var_os("PATH")?)
                .map(|dir| dir.join(DOCKER_PLUGIN))
                .find(|candidate| candidate.is_file())
        });
    let Some(found) = found else {
        assert!(
            env::var_os(REQUIRE_ENV).is_none(),
            "{REQUIRE_ENV} is set, but {DOCKER_PLUGIN} is not on PATH and {DOCKER_PLUGIN_OVERRIDE} \
             is unset"
        );
        eprintln!("skipping: {DOCKER_PLUGIN} is not on PATH and {DOCKER_PLUGIN_OVERRIDE} is unset");
        return None;
    };
    let daemon = std::process::Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !daemon {
        eprintln!("skipping: no Docker daemon answers");
        return None;
    }
    Some(found)
}

/// One run's pieces: the store, its platform records, where it ran.
struct Harness {
    run_id:         RunId,
    run_dir:        PathBuf,
    store:          Arc<MemoryRunStore>,
    records:        Arc<MemoryPlatformRecords>,
    blobs:          Arc<MemoryBlobs>,
    /// The `[run.artifacts] include` patterns the hooks collect under.
    artifacts:      Vec<String>,
    artifact_store: ArtifactStore,
    _root:          tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let artifact_root = root.path().join("artifacts");
        std::fs::create_dir(&artifact_root).expect("the isolated artifact directory creates");
        let artifact_store = ArtifactStore::new(
            Arc::new(
                LocalFileSystem::new_with_prefix(artifact_root)
                    .expect("the local artifact backend builds"),
            ),
            "captures-test",
        );
        Self {
            artifact_store,
            run_id: RunId::new(),
            run_dir: root.path().join("run"),
            store: Arc::new(MemoryRunStore::new()),
            records: Arc::new(MemoryPlatformRecords::new()),
            blobs: Arc::new(MemoryBlobs::new()),
            artifacts: Vec::new(),
            _root: root,
        }
    }

    fn hooks(&self, provider: &SandboxProviderKind) -> HooksSpec {
        HooksSpec {
            records:         Arc::clone(&self.records) as Arc<dyn PlatformRecords>,
            git:             RunGitSettings {
                host_workspaces: *provider == SandboxProviderKind::LOCAL,
                ..RunGitSettings::default()
            },
            artifacts:       self.artifacts.clone(),
            test_gates:      None,
            artifact_writer: Arc::new(StoreArtifactWriter::new(
                self.artifact_store.clone(),
                self.run_id,
            )),
        }
    }

    /// Run the bundle to its end through the engine module, as the worker
    /// does, on the local provider, and report what the record says.
    async fn run(&self, workflow: &str, settings: &str) -> engine::RunOutcome {
        self.run_on(SandboxProviderKind::LOCAL, workflow, settings)
            .await
    }

    /// [`run`](Self::run) on `provider`.
    async fn run_on(
        &self,
        provider: SandboxProviderKind,
        workflow: &str,
        settings: &str,
    ) -> engine::RunOutcome {
        let (interviewer, observers) = no_questions();
        let hooks = self.hooks(&provider);
        let request = RunRequest {
            run_id: self.run_id.to_string(),
            run_dir: self.run_dir.clone(),
            execution: Execution::Start(admit(workflow, settings)),
            store: Arc::clone(&self.store) as Arc<dyn petri_store::RunStore>,
            runtime: RuntimeSpec::default(),
            provider,
            cancel: CancellationToken::new(),
            controls: RunControls::new(),
            interviewer,
            observers,
            secrets: None,
            blobs: Some(Arc::clone(&self.blobs) as Arc<dyn Blobs>),
            hooks: Some(hooks),
        };
        engine::run(request).await.expect("the run executes")
    }

    /// The commits the snapshot repository of `workspace` holds, oldest
    /// first, as `(sha, subject, key)`: every checkpoint's history, whatever
    /// site committed it.
    async fn snapshot_commits(
        &self,
        workspace: &str,
    ) -> Vec<(String, String, Option<CheckpointKey>)> {
        let repository = self.workspaces().snapshot_repository(workspace);
        // Topological, so the linear run history reads parents first even
        // when commits share a timestamp.
        let log = git(&repository, &[
            "log",
            "--topo-order",
            "--reverse",
            "--all",
            "--format=%H%x00%s%x00%B%x1e",
        ])
        .await;
        parse_log(&log)
    }

    async fn inspection(&self) -> RunInspection {
        let logs = self
            .store
            .open(&RunKey::new(self.run_id.to_string()), Access::Read)
            .await
            .expect("the run opens for reading");
        inspect::inspect_run(&*logs)
            .await
            .expect("the stored run inspects")
    }

    fn workspaces(&self) -> RunWorkspaces {
        RunWorkspaces::new(
            self.run_dir.clone(),
            self.run_id.to_string(),
            GitAuthor::default(),
            &RunCheckpointSettings::default(),
        )
    }

    /// The one workspace the run's root scope used.
    async fn workspace(&self) -> String {
        let mut entries = fs::read_dir(self.run_dir.join("scopes"))
            .await
            .expect("the scopes directory exists");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("an entry reads") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names.len(), 1, "one workspace: {names:?}");
        names.remove(0)
    }

    fn workspace_path(&self, workspace: &str) -> PathBuf {
        self.workspaces().workspace_path(workspace)
    }

    /// The checkpoint records, in seq order, as `(key, sha)`.
    fn checkpoints(&self) -> Vec<(CheckpointKey, String)> {
        self.records
            .records(&self.run_id)
            .into_iter()
            .filter_map(|stored| match stored.record {
                PlatformRecord::Checkpoint(record) => Some((
                    CheckpointKey::from_operation(record.operation.as_ref()?)?,
                    record.git_commit_sha?,
                )),
                _ => None,
            })
            .collect()
    }

    async fn recover(&self) -> Recovery {
        recovery::recover(RecoveryRequest {
            run_id:  self.run_id,
            run_dir: self.run_dir.clone(),
            store:   Arc::clone(&self.store) as Arc<dyn petri_store::RunStore>,
            records: Arc::clone(&self.records) as Arc<dyn PlatformRecords>,
            git:     RunGitSettings::default(),
        })
        .await
        .expect("recovery decides")
    }
}

/// An interviewer for a run that asks nothing, with its expiry observer.
fn no_questions() -> (
    Arc<dyn petri_execution::Interviewer>,
    Vec<Arc<dyn petri_execution::ExecutionObserver>>,
) {
    let interviewer = support::no_questions(Arc::new(support::Silent));
    let observers = vec![interviewer.observer()];
    (Arc::new(interviewer), observers)
}

/// `git` in a workspace, its stdout.
async fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .await
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The commits on the run branch, oldest first, as `(sha, subject, key)`.
async fn commits(path: &Path) -> Vec<(String, String, Option<CheckpointKey>)> {
    let log = git(path, &["log", "--reverse", "--format=%H%x00%s%x00%B%x1e"]).await;
    parse_log(&log)
}

fn parse_log(log: &str) -> Vec<(String, String, Option<CheckpointKey>)> {
    log.split('\u{1e}')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            let mut parts = entry.trim_start().splitn(3, '\0');
            let sha = parts.next().unwrap_or_default().to_string();
            let subject = parts.next().unwrap_or_default().to_string();
            let body = parts.next().unwrap_or_default();
            (sha, subject, CheckpointKey::from_message(body))
        })
        .collect()
}

/// The stages that ran, by node name, with their final status.
fn stages(inspection: &RunInspection) -> Vec<(String, String)> {
    inspection
        .executions
        .iter()
        .filter_map(|execution| execution.engine.as_ref())
        .flat_map(|engine| engine.history.iter())
        .map(|record| (record.node.to_string(), record.status.to_string()))
        .collect()
}

/// Every finished stage is committed on the run branch with the identity
/// trailers, its platform record names the commit, and the run-end hooks
/// reached Petri's local service through Fabro's wrapper.
#[tokio::test]
async fn every_finish_is_committed_and_recorded() {
    if host_plugin().is_none() {
        return;
    }
    let harness = Harness::new();
    let workflow = workflow(
        "  write [shape=parallelogram, script=\"echo one > out.txt\"]\n  check \
         [shape=parallelogram, script=\"test \\\"$(cat out.txt)\\\" = one\"]",
        "  start -> write -> check -> exit",
    );
    let settings = format!(
        "{SETTINGS}\n[[run.hooks]]\nevent = \"run_complete\"\nscript = \"echo run_complete >> \
         run-end.log\"\n\n[[run.hooks]]\nevent = \"sandbox_cleanup\"\nscript = \"echo \
         sandbox_cleanup >> run-end.log\"\n"
    );
    let outcome = harness.run(&workflow, &settings).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);

    let workspace = harness.workspace().await;
    let path = harness.workspace_path(&workspace);
    assert_eq!(
        git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).await,
        format!("fabro/run/{}", harness.run_id)
    );
    let commits = commits(&path).await;
    let subjects: Vec<&str> = commits
        .iter()
        .map(|(_, subject, _)| subject.as_str())
        .collect();
    let run_id = harness.run_id.to_string();
    assert_eq!(subjects, vec![
        format!("fabro({run_id}): start (success)"),
        format!("fabro({run_id}): write (success)"),
        format!("fabro({run_id}): check (success)"),
        format!("fabro({run_id}): exit (success)"),
    ]);
    assert!(
        commits.iter().all(|(_, _, key)| key.is_some()),
        "every commit carries its key: {commits:?}"
    );

    let checkpoints = harness.checkpoints();
    assert_eq!(checkpoints.len(), 4, "{checkpoints:?}");
    let by_sha: Vec<&String> = checkpoints.iter().map(|(_, sha)| sha).collect();
    let committed: Vec<&String> = commits.iter().map(|(sha, _, _)| sha).collect();
    assert_eq!(by_sha, committed, "each record names its stage's commit");
    for ((key, _), (_, _, trailer)) in checkpoints.iter().zip(&commits) {
        assert_eq!(Some(*key), *trailer);
    }
    assert!(
        checkpoints.iter().all(|(key, _)| key.execution == 0),
        "{checkpoints:?}"
    );

    // The snapshot repository holds every checkpoint.
    let published = harness
        .workspaces()
        .published(&workspace)
        .await
        .expect("the snapshots list");
    assert_eq!(published.len(), 4);

    // `run_complete` and `sandbox_cleanup` ran through the forwarded
    // service, with the sandbox in place.
    let run_end = fs::read_to_string(path.join("run-end.log"))
        .await
        .expect("the run-end hooks wrote their log");
    assert_eq!(run_end, "run_complete\nsandbox_cleanup\n");
}

/// The files under `[run.artifacts] include` are collected once per
/// content into configured artifact storage, the run branch and the author
/// identity are recorded when the branch is created, every checkpoint after the
/// first carries its diff from its parent, and the run's diff is recorded at
/// the end.
#[tokio::test]
async fn artifacts_the_branch_and_the_diffs_are_recorded() {
    if host_plugin().is_none() {
        return;
    }
    let mut harness = Harness::new();
    harness.artifacts = vec!["assets/**".to_string()];
    let workflow = workflow(
        "  write [shape=parallelogram, script=\"mkdir -p assets && printf one > \
         assets/report.txt && echo line > story.txt\"]\n  keep [shape=parallelogram, \
         script=\"test -f assets/report.txt\"]\n  change [shape=parallelogram, script=\"printf \
         two > assets/report.txt\"]",
        "  start -> write -> keep -> change -> exit",
    );
    let outcome = harness.run(&workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");

    let records = harness.records.records(&harness.run_id);
    let artifacts: Vec<_> = records
        .iter()
        .filter_map(|stored| match &stored.record {
            PlatformRecord::ArtifactCollected(record) => Some(record.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(artifacts.len(), 2, "one capture per content: {artifacts:?}");
    assert!(
        artifacts
            .iter()
            .all(|artifact| artifact.path == "assets/report.txt"),
        "{artifacts:?}"
    );
    assert_eq!(artifacts[0].bytes, 3);
    assert_eq!(artifacts[0].digest, artifacts[0].source.hash().to_string());
    assert_ne!(artifacts[0].digest, artifacts[1].digest);
    assert!(
        artifacts
            .iter()
            .all(|artifact| matches!(artifact.source, fabro_types::ArtifactSource::ObjectStore(_)))
    );
    assert!(
        harness
            .blobs
            .read(&artifacts[1].source.hash())
            .await
            .unwrap()
            .is_none()
    );
    let bytes = harness
        .artifact_store
        .get_capture(&harness.run_id, &artifacts[1].source.hash())
        .await
        .expect("the object reads")
        .expect("the object exists");
    assert_eq!(bytes.as_ref(), b"two");
    // The first capture belongs to `write`, the second to `change`; `keep`
    // saw the file unchanged and recorded nothing.
    let checkpoint_firings: Vec<(String, u64)> = checkpoint_nodes(&harness).await;
    let firing = |node: &str| {
        checkpoint_firings
            .iter()
            .find(|(name, _)| name == node)
            .map(|(_, firing)| *firing)
            .expect("the node checkpointed")
    };
    assert_eq!(artifacts[0].firing, firing("write"));
    assert_eq!(artifacts[1].firing, firing("change"));

    let branches: Vec<_> = records
        .iter()
        .filter_map(|stored| match &stored.record {
            PlatformRecord::RunBranch(record) => Some(record.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(branches.len(), 1, "{branches:?}");
    let workspace = harness.workspace().await;
    assert_eq!(
        branches[0].run_branch.as_deref(),
        Some(format!("fabro/run/{}", harness.run_id).as_str())
    );
    assert_eq!(branches[0].workspace.as_deref(), Some(workspace.as_str()));
    let checkpoints = harness.checkpoints();
    assert_eq!(
        branches[0].base_sha.as_deref(),
        Some(checkpoints[0].1.as_str()),
        "a branch in a fresh repository starts from its first checkpoint"
    );
    let identities: Vec<_> = records
        .iter()
        .filter_map(|stored| match &stored.record {
            PlatformRecord::GitIdentity(record) => Some(record.identity.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(identities.len(), 1, "{identities:?}");
    assert_eq!(identities[0].source, GitIdentitySource::Default);
    assert_eq!(identities[0].name, GitAuthor::default().name);

    // The checkpoints carry their diffs: `start` is the root commit and has
    // none; `write` adds two files; `keep` changes nothing; `change` edits
    // one file.
    let diffs: Vec<_> = records
        .iter()
        .filter_map(|stored| match &stored.record {
            PlatformRecord::Checkpoint(record) => {
                Some((record.diff_summary, record.patch_blob.is_some()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(diffs.len(), 5, "{diffs:?}");
    assert_eq!(diffs[0], (None, false));
    let write = diffs[1].0.expect("the write diff");
    assert_eq!(
        (write.files_changed, write.additions, write.deletions),
        (2, 2, 0)
    );
    assert!(diffs[1].1, "the write patch is a blob");
    let keep = diffs[2].0.expect("the keep diff");
    assert_eq!(keep.files_changed, 0);
    assert!(!diffs[2].1, "an empty diff has no patch blob");
    let change = diffs[3].0.expect("the change diff");
    assert_eq!(
        (change.files_changed, change.additions, change.deletions),
        (1, 1, 1)
    );

    let run_diffs: Vec<_> = records
        .iter()
        .filter_map(|stored| match &stored.record {
            PlatformRecord::RunDiff(record) => Some(record.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(run_diffs.len(), 1, "{run_diffs:?}");
    let run_diff = &run_diffs[0];
    assert_eq!(run_diff.base_sha, branches[0].base_sha);
    assert_eq!(
        run_diff.head_sha.as_deref(),
        Some(checkpoints.last().expect("checkpoints").1.as_str())
    );
    let summary = run_diff.diff_summary.expect("the run diff summary");
    assert_eq!((summary.files_changed, summary.additions), (2, 2));
    let patch = harness
        .blobs
        .read(&run_diff.patch_blob.expect("the run patch is a blob"))
        .await
        .expect("the blob reads")
        .expect("the blob exists");
    let patch = String::from_utf8_lossy(&patch);
    assert!(patch.contains("+two"), "{patch}");
    assert!(patch.contains("+line"), "{patch}");
}

/// The node of every checkpoint record, in record order, with its firing.
async fn checkpoint_nodes(harness: &Harness) -> Vec<(String, u64)> {
    let inspection = harness.inspection().await;
    let history: Vec<(u64, String)> = inspection
        .executions
        .iter()
        .filter_map(|execution| execution.engine.as_ref())
        .flat_map(|engine| engine.history.iter())
        .map(|record| (record.firing, record.node.to_string()))
        .collect();
    harness
        .records
        .records(&harness.run_id)
        .into_iter()
        .filter_map(|stored| match stored.record {
            PlatformRecord::Checkpoint(record) => {
                let node = history
                    .iter()
                    .find(|(firing, _)| *firing == record.firing)
                    .map(|(_, node)| node.clone())?;
                Some((node, record.firing))
            }
            _ => None,
        })
        .collect()
}

/// A stage that fails on its own terms is committed like a successful one,
/// and its failure route runs on the committed files.
#[tokio::test]
async fn a_failed_stage_is_committed_and_its_route_sees_the_files() {
    if host_plugin().is_none() {
        return;
    }
    let harness = Harness::new();
    let workflow = workflow(
        "  work [shape=parallelogram, script=\"echo partial > out.txt; exit 1\"]\n  fix \
         [shape=parallelogram, script=\"test \\\"$(cat out.txt)\\\" = partial && echo fixed >> \
         out.txt\"]",
        "  start -> work -> exit\n  work -> fix [condition=\"outcome=failed\"]\n  fix -> exit",
    );
    let outcome = harness.run(&workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let inspection = harness.inspection().await;
    assert_eq!(stages(&inspection), vec![
        ("start".to_string(), "success".to_string()),
        ("work".to_string(), "failure".to_string()),
        ("fix".to_string(), "success".to_string()),
        ("exit".to_string(), "success".to_string()),
    ]);

    let workspace = harness.workspace().await;
    let path = harness.workspace_path(&workspace);
    let commits = commits(&path).await;
    let run_id = harness.run_id.to_string();
    assert_eq!(commits[1].1, format!("fabro({run_id}): work (failure)"));
    assert_eq!(
        git(&path, &["show", &format!("{}:out.txt", commits[1].0)]).await,
        "partial",
        "the failed stage's files are in its snapshot"
    );
    assert_eq!(
        git(&path, &["show", &format!("{}:out.txt", commits[2].0)]).await,
        "partial\nfixed",
        "the route ran on the committed files"
    );
    assert_eq!(harness.checkpoints().len(), 4);
}

/// A checkpoint commit that fails is fatal: the stage's outcome is recorded
/// as `checkpoint_failed`, no route is taken, the run ends failed with the
/// checkpoint's error, and a restart reports it failed without resuming.
#[tokio::test]
async fn a_failed_checkpoint_ends_the_run_with_no_route() {
    if host_plugin().is_none() {
        return;
    }
    let harness = Harness::new();
    let workflow = workflow(
        "  wreck [shape=parallelogram, script=\"rm -rf .git && echo garbage > .git && echo wrecked \
         > out.txt\"]\n  next [shape=parallelogram, script=\"echo next > next.txt\"]\n  fix \
         [shape=parallelogram, script=\"echo fix > fix.txt\"]",
        "  start -> wreck -> next -> exit\n  wreck -> fix [condition=\"outcome=failed\"]\n  fix -> \
         exit",
    );
    let outcome = harness.run(&workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Failed, "{outcome:?}");
    let failure = outcome
        .failure
        .clone()
        .expect("the run failed with a reason");
    assert!(
        failure.contains("checkpoint commit of `wreck` failed"),
        "{failure}"
    );

    let inspection = harness.inspection().await;
    let attempts: Vec<_> = inspection
        .executions
        .iter()
        .filter_map(|execution| execution.engine.as_ref())
        .flat_map(|engine| engine.attempts.iter())
        .collect();
    let wreck = attempts
        .iter()
        .find(|attempt| attempt.node.as_deref() == Some("wreck"))
        .expect("the wrecked stage finished");
    assert_eq!(wreck.status, "failure");
    assert_eq!(
        wreck.failure.as_ref().map(|failure| failure.class.as_str()),
        Some(CHECKPOINT_FAILED_CLASS),
        "{wreck:?}"
    );
    assert!(
        attempts
            .iter()
            .all(|attempt| !matches!(attempt.node.as_deref(), Some("next" | "fix"))),
        "no route ran: {:?}",
        stages(&inspection)
    );
    let workspace = harness.workspace().await;
    let path = harness.workspace_path(&workspace);
    assert!(!fs::try_exists(path.join("next.txt")).await.expect("exists"));
    assert!(!fs::try_exists(path.join("fix.txt")).await.expect("exists"));
    // The wrecked stage has no record: its commit never landed.
    let checkpoints = harness.checkpoints();
    assert_eq!(checkpoints.len(), 1, "{checkpoints:?}");

    // A restart finds the failed checkpoint and reports the run failed.
    let recovery = harness.recover().await;
    assert!(
        matches!(&recovery, Recovery::Failed { reason } if reason.contains("checkpoint of wreck")),
        "{recovery:?}"
    );
}

/// The two ends of recovery that need no crash: a run the store never held
/// starts over, and a run that finished has nothing to bring back.
#[tokio::test]
async fn recovery_starts_an_unknown_run_and_resumes_a_finished_one() {
    if host_plugin().is_none() {
        return;
    }
    let harness = Harness::new();
    assert_eq!(harness.recover().await, Recovery::Start);

    let workflow = workflow(
        "  write [shape=parallelogram, script=\"echo one > out.txt\"]",
        "  start -> write -> exit",
    );
    let outcome = harness.run(&workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert_eq!(harness.recover().await, Recovery::Resume {
        workspaces: Vec::new(),
    });
    assert_eq!(
        harness
            .records
            .read_kind(&harness.run_id, PlatformRecordKind::Checkpoint)
            .await
            .expect("the records read")
            .len(),
        3
    );
}

/// A `[[run.hooks]]` hook that blocks a tool effect keeps working through
/// the forwarded local service: the agent's `rm` is refused by the
/// `pre_tool_use` hook, the model is told why, and the file it aimed at is
/// still in the stage's snapshot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_hook_blocks_a_tool_effect_through_the_forwarded_service() {
    use fabro_auth::test_support::env_credential_source;
    use fabro_llm::test_support::test_catalog_with_provider_base_url;
    use fabro_petri::runtime;
    use fabro_test::{TwinScenario, TwinScenarios, TwinToolCall};
    use lithos_llm::catalog::ProviderId;
    use serde_json::json;

    const MODEL: &str = "gpt-5.6-sol";
    if host_plugin().is_none() {
        return;
    }
    let twin = fabro_test::twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(namespace.clone())
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains("Remove the scratch file")
                .tool_call(TwinToolCall::new(
                    "shell_command",
                    json!({ "command": "rm -f scratch.txt && echo removed" }),
                )),
        )
        .scenario(
            TwinScenario::responses(MODEL)
                .input_contains("destructive commands are not allowed")
                .text("Understood, the file stays."),
        )
        .load(twin)
        .await;
    let api_key = namespace.clone();
    let credentials =
        env_credential_source(move |name| (name == "OPENAI_API_KEY").then(|| api_key.clone()));
    let client = runtime::model_client(
        test_catalog_with_provider_base_url("openai", &twin.base_url),
        credentials,
        None,
        &[ProviderId::new("openai")],
    )
    .expect("the model client builds")
    .expect("openai is eligible");

    let harness = Harness::new();
    let (interviewer, observers) = no_questions();
    let workflow = format!(
        "digraph Hooks {{\n  graph [backend=\"api\", goal=\"Check the tool hooks\", \
         default_max_retries=0]\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  seed \
         [shape=parallelogram, script=\"echo keep > scratch.txt\"]\n  agent [prompt=\"Remove the \
         scratch file with the shell tool.\", model=\"{MODEL}\", provider=\"openai\", \
         fidelity=\"full\"]\n  check [shape=parallelogram, script=\"test \\\"$(cat scratch.txt)\\\" \
         = keep\"]\n  start -> seed -> agent -> check -> exit\n}}\n"
    );
    let settings = format!(
        "{SETTINGS}\n[[run.hooks]]\nname = \"no-destruction\"\nevent = \"pre_tool_use\"\nmatcher \
         = \"shell\"\nscript = \"if grep -q 'rm ' \\\"$FABRO_HOOK_CONTEXT\\\"; then echo \
         '{{\\\"decision\\\":\\\"block\\\",\\\"reason\\\":\\\"destructive commands are not \
         allowed\\\"}}'; exit 2; fi\"\n"
    );
    let request = RunRequest {
        run_id: harness.run_id.to_string(),
        run_dir: harness.run_dir.clone(),
        execution: Execution::Start(admit(&workflow, &settings)),
        store: Arc::clone(&harness.store) as Arc<dyn petri_store::RunStore>,
        runtime: RuntimeSpec {
            model_client: Some(client),
            ..RuntimeSpec::default()
        },
        provider: SandboxProviderKind::LOCAL,
        cancel: CancellationToken::new(),
        controls: RunControls::new(),
        interviewer,
        observers,
        secrets: None,
        blobs: None,
        hooks: Some(harness.hooks(&SandboxProviderKind::LOCAL)),
    };
    let outcome = engine::run(request).await.expect("the run executes");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let inspection = harness.inspection().await;
    assert_eq!(stages(&inspection), vec![
        ("start".to_string(), "success".to_string()),
        ("seed".to_string(), "success".to_string()),
        ("agent".to_string(), "success".to_string()),
        ("check".to_string(), "success".to_string()),
        ("exit".to_string(), "success".to_string()),
    ]);
    let requests = twin.request_logs(&namespace).await;
    let inputs: Vec<&str> = requests["requests"]
        .as_array()
        .map(|requests| {
            requests
                .iter()
                .filter_map(|request| request["input_text"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(inputs.len(), 2, "{inputs:?}");
    assert!(
        inputs[1].contains("destructive commands are not allowed"),
        "the model was told why the tool was blocked: {inputs:?}"
    );
    let workspace = harness.workspace().await;
    let path = harness.workspace_path(&workspace);
    assert_eq!(
        git(&path, &["show", "HEAD:scratch.txt"]).await,
        "keep",
        "the blocked removal never happened"
    );
    assert_eq!(harness.checkpoints().len(), 5);
}

/// The run's records name the workspace its root invocation ran in: what
/// recovery reads to find the workspace of a live execution.
#[tokio::test]
async fn the_records_name_the_root_invocations_workspace() {
    use fabro_petri::workspace::WorkspaceLookup;
    use petri_execution::InvocationId;

    if host_plugin().is_none() {
        return;
    }
    let harness = Harness::new();
    let workflow = workflow(
        "  write [shape=parallelogram, script=\"echo one > out.txt\"]",
        "  start -> write -> exit",
    );
    let outcome = harness.run(&workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let lookup = WorkspaceLookup::new(
        Arc::clone(&harness.store) as Arc<dyn petri_store::RunStore>,
        RunKey::new(harness.run_id.to_string()),
    );
    let named = lookup
        .of_invocation(InvocationId::ROOT)
        .await
        .expect("the lookup reads the records");
    assert_eq!(named, vec![harness.workspace().await]);
    let inspection = harness.inspection().await;
    let statuses: Vec<&str> = inspection
        .executions
        .iter()
        .map(|execution| execution.status)
        .collect();
    assert_eq!(statuses, vec!["finished"]);
}

/// The branches of a parallel node share their caller's workspace: their
/// checkpoints are committed one at a time on the one run branch, every
/// firing of every execution gets its record, and no transition reports a
/// problem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_branches_checkpoint_the_shared_workspace_in_turn() {
    if host_plugin().is_none() {
        return;
    }
    let harness = Harness::new();
    let workflow = workflow(
        "  fork [shape=component]\n  a [shape=parallelogram, script=\"echo a > a.txt\"]\n  b \
         [shape=parallelogram, script=\"echo b > b.txt\"]\n  merge [shape=tripleoctagon]\n  check \
         [shape=parallelogram, script=\"test -f a.txt && test -f b.txt\"]",
        "  start -> fork\n  fork -> a\n  fork -> b\n  a -> merge\n  b -> merge\n  merge -> check -> \
         exit",
    );
    let outcome = harness.run(&workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");

    let records = support::all_records(harness.store.as_ref(), &harness.run_id.to_string()).await;
    let problems: Vec<&serde_json::Value> = records
        .iter()
        .filter_map(|record| record.pointer("/event/$note"))
        .filter(|note| note["kind"] == "transition" && !note["payload"]["problems"].is_null())
        .collect();
    assert!(problems.is_empty(), "transition problems: {problems:#?}");

    let inspection = harness.inspection().await;
    let mut finished: Vec<CheckpointKey> = inspection
        .executions
        .iter()
        .filter_map(|execution| Some((execution.execution.raw(), execution.engine.as_ref()?)))
        .flat_map(|(execution, engine)| {
            engine.attempts.iter().map(move |attempt| CheckpointKey {
                execution,
                firing: attempt.firing,
                attempt: attempt.attempt,
            })
        })
        .collect();
    finished.sort();
    let mut recorded: Vec<CheckpointKey> = harness
        .checkpoints()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    recorded.sort();
    assert_eq!(
        recorded, finished,
        "every finish of every execution is recorded"
    );
    assert_eq!(inspection.executions.len(), 3, "the root and two branches");
    assert_eq!(harness.workspace().await, "invocation-0-scope-0");
}

/// On Docker the workspace lives inside the container: every finished
/// stage is committed there, the commit leaves the container as a bundle,
/// and the snapshot repository on the host holds each checkpoint under
/// its ref, with the platform records naming the same commits.
#[tokio::test]
async fn a_docker_run_commits_inside_the_container_and_publishes_every_checkpoint() {
    if docker_plugin().is_none() {
        return;
    }
    assert_sandbox_run_publishes_every_checkpoint(SandboxProviderKind::DOCKER).await;
}

/// The same protocol on Daytona: the sandbox-driver facets are provider
/// neutral, so the commit, the bundle and the restore take one path. Live:
/// it needs `DAYTONA_API_KEY` and the Daytona plugin, and provisions a
/// sandbox.
#[tokio::test]
#[ignore = "requires live Daytona credentials and provisions a sandbox"]
async fn a_daytona_run_commits_inside_the_sandbox_and_publishes_every_checkpoint() {
    assert!(
        env::var_os("DAYTONA_API_KEY").is_some(),
        "DAYTONA_API_KEY must be set to run this live test"
    );
    assert_sandbox_run_publishes_every_checkpoint(SandboxProviderKind::DAYTONA).await;
}

/// Bytes of incompressible data the first stage writes: past the plugin
/// transport's 16 MiB cap on one file read, so its bundle leaves the
/// sandbox in more than one part.
const LARGE_FILE_BYTES: usize = 20 * 1024 * 1024;

/// A two-stage run on `provider`, whose workspace lives inside a sandbox:
/// nothing of it is on the host, every checkpoint is published, and the
/// bundles carried the stages' files, a large one in parts.
async fn assert_sandbox_run_publishes_every_checkpoint(provider: SandboxProviderKind) {
    let harness = Harness::new();
    let workflow = workflow(
        &format!(
            "  write [shape=parallelogram, script=\"echo one > out.txt && head -c \
             {LARGE_FILE_BYTES} /dev/urandom > large.bin\"]\n  check [shape=parallelogram, \
             script=\"test \\\"$(cat out.txt)\\\" = one && git log --format=%s | head -1 | grep -q \
             write\"]"
        ),
        "  start -> write -> check -> exit",
    );
    let outcome = harness.run_on(provider, &workflow, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(outcome.complete, "{:?}", outcome.incomplete);

    let checkpoints = harness.checkpoints();
    assert_eq!(checkpoints.len(), 4, "{checkpoints:?}");
    let workspace = "invocation-0-scope-0";
    assert!(
        !harness.workspaces().workspace_exists(workspace).await,
        "nothing of the workspace is on the host"
    );
    let published = harness
        .workspaces()
        .published(workspace)
        .await
        .expect("the snapshot repository lists");
    let mut by_key: Vec<(CheckpointKey, String)> = published
        .iter()
        .map(|snapshot| (snapshot.key, snapshot.sha.clone()))
        .collect();
    let mut recorded = checkpoints.clone();
    recorded.sort();
    by_key.sort();
    assert_eq!(by_key, recorded, "every record names a published snapshot");

    let commits = harness.snapshot_commits(workspace).await;
    let subjects: Vec<&str> = commits
        .iter()
        .map(|(_, subject, _)| subject.as_str())
        .collect();
    let run_id = harness.run_id.to_string();
    assert_eq!(subjects, vec![
        format!("fabro({run_id}): start (success)"),
        format!("fabro({run_id}): write (success)"),
        format!("fabro({run_id}): check (success)"),
        format!("fabro({run_id}): exit (success)"),
    ]);
    let (write_sha, _, _) = &commits[1];
    let repository = harness.workspaces().snapshot_repository(workspace);
    assert_eq!(
        git(&repository, &["show", &format!("{write_sha}:out.txt")]).await,
        "one",
        "the bundle carried the stage's files"
    );
    assert_eq!(
        git(&repository, &[
            "cat-file",
            "-s",
            &format!("{write_sha}:large.bin")
        ])
        .await,
        LARGE_FILE_BYTES.to_string(),
        "the large file came through the split transfer whole"
    );
}
