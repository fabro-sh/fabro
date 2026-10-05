//! Fabro's hooks on a Petri run, in process: command-only bundles run
//! through `engine::run` on the host sandbox with the memory store and
//! in-memory platform records, and the checkpoint commit, its record, the
//! failure route, the fatal checkpoint, and the run-end hooks are checked
//! against the workspace's Git history and the run's records.
//!
//! Built-in Host scopes run in process without a plugin executable.
//! The crash cases need a worker to kill and live in the CLI's scenario suite.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests inspect backend availability and read the workspace's history with git"
)]

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
    CHECKPOINT_FAILED_CLASS, CheckpointKey, RunGitSettings, RunWorkspaces, Site,
};
use fabro_petri::controls::RunControls;
use fabro_petri::engine::{self, Execution, RunRequest, RunStatus};
use fabro_petri::fork;
use fabro_petri::hooks::{HooksSpec, Publication, RunPublisher};
use fabro_petri::platform_records::{PlatformRecordError, PlatformRecords};
use fabro_petri::providers::{DaytonaCredentials, SandboxProviderConfig};
use fabro_petri::prune::{self, PruneRequest};
use fabro_petri::recovery::{self, Recovery, RecoveryRequest};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::source::{RunSource, SourceRevision};
use fabro_petri::test_support::{MemoryBlobs, MemoryPlatformRecords};
use fabro_store::{ArtifactStore, PlatformRecord, PlatformRecordKind};
use fabro_types::settings::run::{
    EnvironmentNetworkSettings, EnvironmentResourcesSettings, RunCheckpointSettings, RunNamespace,
};
use fabro_types::{GitIdentitySource, RunId, SandboxProviderKind};
use object_store::local::LocalFileSystem;
use petri_execution::inspect::{self, RunInspection};
use petri_store::{Access, MemoryRunStore, RunKey, RunStore as _};
use tokio::process::Command;
use tokio::sync::{Notify, Semaphore};
use tokio::{fs, time};
use tokio_util::sync::CancellationToken;

mod support;

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
    /// Where the run's workspaces are checked out from.
    source:         Option<RunSource>,
    /// Treat the local provider's workspaces as a sandbox's: `git` runs
    /// through the scope's environment and checkpoints leave as bundles.
    sandboxed:      bool,
    /// What a successful run's work does when it ends.
    publisher:      Option<Arc<dyn RunPublisher>>,
    fail_run_diff:  bool,
    _root:          tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
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
        let (origin, _) = upstream(&root.path().join("fixture"), 1).await;
        Self {
            artifact_store,
            run_id: RunId::new(),
            run_dir: root.path().join("run"),
            store: Arc::new(MemoryRunStore::new()),
            records: Arc::new(MemoryPlatformRecords::new()),
            blobs: Arc::new(MemoryBlobs::new()),
            artifacts: Vec::new(),
            source: Some(file_source(&origin, "main", None)),
            sandboxed: false,
            publisher: None,
            fail_run_diff: false,
            _root: root,
        }
    }

    fn hooks(&self, provider: &SandboxProviderKind) -> HooksSpec {
        HooksSpec {
            records:         if self.fail_run_diff {
                Arc::new(RejectRunDiff(self.records.clone()))
            } else {
                self.records.clone()
            },
            git:             RunGitSettings {
                host_workspaces: *provider == SandboxProviderKind::LOCAL && !self.sandboxed,
                ..RunGitSettings::default()
            },
            artifacts:       self.artifacts.clone(),
            test_gates:      None,
            artifact_writer: Arc::new(StoreArtifactWriter::new(self.artifact_store.clone())),
            source:          self.source.clone(),
            publisher:       self.publisher.clone(),
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
        self.execute_on(provider, workflow, settings, false).await
    }

    async fn execute_on(
        &self,
        provider: SandboxProviderKind,
        workflow: &str,
        settings: &str,
        resumed: bool,
    ) -> engine::RunOutcome {
        let (interviewer, observers) = no_questions();
        let hooks = self.hooks(&provider);
        let daytona = (provider == SandboxProviderKind::DAYTONA).then(|| {
            DaytonaCredentials::from_api_key(
                env::var("DAYTONA_API_KEY").expect("live Daytona credentials"),
                |name| env::var(name).ok(),
            )
        });
        let sandbox = SandboxProviderConfig::from_lookup(daytona, |name| env::var(name).ok());
        let request = RunRequest {
            run_id: self.run_id.to_string(),
            run_dir: self.run_dir.clone(),
            execution: if resumed {
                Execution::Resume
            } else {
                Execution::Start(admit(workflow, settings))
            },
            store: Arc::clone(&self.store) as Arc<dyn petri_store::RunStore>,
            runtime: RuntimeSpec {
                sandbox,
                ..RuntimeSpec::default()
            },
            provider,
            resources: EnvironmentResourcesSettings::default(),
            network: EnvironmentNetworkSettings::default(),
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
            store:   Arc::clone(&self.store) as Arc<dyn petri_store::RunStore>,
            records: Arc::clone(&self.records) as Arc<dyn PlatformRecords>,
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
        .filter(|(_, _, key)| key.is_some())
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

/// A dry run keeps checkpointable local workspaces even when the selected
/// environment would require Docker or Daytona. Its command is simulated.
#[tokio::test]
async fn dry_runs_use_local_workspaces_and_checkpoint_without_sandbox_credentials() {
    for provider in [SandboxProviderKind::DOCKER, SandboxProviderKind::DAYTONA] {
        let mut harness = Harness::new().await;
        harness.source = None;
        let workflow = workflow(
            r#"  write [shape=parallelogram, script="touch should-not-exist; exit 1"]"#,
            "  start -> write -> exit",
        );
        let settings = format!(
            "{SETTINGS}\n[run.environment]\nid = \"remote\"\n\n[environments.remote]\nprovider = \"{provider}\"\n\n[environments.remote.image]\ndocker = \"invalid.example/dry-run:must-not-pull\"\n"
        );
        let runtime = RuntimeSpec {
            dry_run: true,
            ..RuntimeSpec::default()
        };
        let graphs = support::admit(
            &[("workflow.fabro", &workflow), ("workflow.toml", &settings)],
            Launch::default(),
            &runtime,
        );
        let mut request = support::run_request(
            &harness.run_id.to_string(),
            &harness.run_dir,
            graphs,
            harness.store.clone(),
            runtime,
            support::no_questions(Arc::new(support::Silent)),
        );
        request.hooks = Some(harness.hooks(&provider));
        request.provider = provider.clone();
        request.blobs = Some(harness.blobs.clone());
        let outcome = engine::run(request).await.expect("the dry run executes");
        assert_eq!(
            outcome.status,
            RunStatus::Success,
            "{provider}: {outcome:?}"
        );
        assert!(outcome.complete, "{provider}: {:?}", outcome.incomplete);
        let workspace = harness.workspace().await;
        assert!(
            !harness
                .workspace_path(&workspace)
                .join("should-not-exist")
                .exists()
        );
        assert_eq!(
            harness.checkpoints().len(),
            3,
            "{provider}: every stage checkpoints"
        );
        assert_eq!(commits(&harness.workspace_path(&workspace)).await.len(), 3);
    }
}

/// Every finished stage is committed on the run branch with the identity
/// trailers, its platform record names the commit, and the run-end hooks
/// reached Petri's local service through Fabro's wrapper.
#[tokio::test]
async fn every_finish_is_committed_and_recorded() {
    let harness = Harness::new().await;
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

    assert!(!harness.run_dir.join("snapshots").exists());

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
    let mut harness = Harness::new().await;
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
    assert_ne!(artifacts[0].source.hash(), artifacts[1].source.hash());
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
        Some(
            git(&harness.workspace_path(&workspace), &[
                "rev-parse",
                &format!("{}^", checkpoints[0].1)
            ])
            .await
            .as_str()
        ),
        "the branch starts at the upstream commit"
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
    assert_eq!(diffs[0].0.unwrap().files_changed, 0);
    assert!(!diffs[0].1);
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
    let harness = Harness::new().await;
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
    let harness = Harness::new().await;
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
    let harness = Harness::new().await;
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

    let harness = Harness::new().await;
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
        resources: EnvironmentResourcesSettings::default(),
        network: EnvironmentNetworkSettings::default(),
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

    let harness = Harness::new().await;
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
    let harness = Harness::new().await;
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
    let child = recorded.iter().find(|key| key.execution != 0).unwrap();
    let refused = fork::check(
        harness.store.as_ref(),
        harness.run_id,
        fork::position(child.execution, child.firing),
    )
    .await
    .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("inside a child invocation cannot be forked")
    );
}

/// On Docker the workspace lives inside the container: every finished
/// stage is committed there, the commit leaves the container as a bundle,
/// and the snapshot repository on the host holds each checkpoint under
/// its ref, with the platform records naming the same commits.
#[tokio::test]
async fn a_docker_run_commits_inside_the_container_and_publishes_every_checkpoint() {
    if !fabro_test::docker_available() {
        return;
    }
    assert_sandbox_run_publishes_every_checkpoint(SandboxProviderKind::DOCKER).await;
}

/// The same protocol on Daytona: the sandbox-driver facets are provider
/// neutral, so the commit, the bundle and the restore take one path. Live:
/// it needs `DAYTONA_API_KEY`, passed explicitly to the provider, and
/// provisions a sandbox.
#[tokio::test]
#[ignore = "requires live Daytona credentials and provisions a sandbox"]
async fn a_daytona_run_commits_inside_the_sandbox_and_publishes_every_checkpoint() {
    assert!(
        env::var_os("DAYTONA_API_KEY").is_some(),
        "DAYTONA_API_KEY must be set to run this live test"
    );
    assert_sandbox_run_publishes_every_checkpoint(SandboxProviderKind::DAYTONA).await;
}

/// Git commits and pushes use the acquired sandbox, without a host repository.
async fn assert_sandbox_run_publishes_every_checkpoint(provider: SandboxProviderKind) {
    let mut harness = Harness::new().await;
    harness.source = Some(RunSource {
        origin:      "https://github.com/octocat/Hello-World.git".to_string(),
        revision:    SourceRevision::Branch("master".to_string()),
        branch:      "master".to_string(),
        depth:       Some(1),
        credentials: None,
    });
    let publisher = RecordingPublisher::new(None);
    harness.publisher = Some(publisher.clone());
    let workflow = workflow(
        r#"  write [shape=parallelogram, script="echo one > out.txt"]
  check [shape=parallelogram, script="test -f out.txt && git log --format=%s | head -1 | grep -q write"]"#,
        "  start -> write -> check -> exit",
    );
    let outcome = harness.run_on(provider.clone(), &workflow, SETTINGS).await;
    if provider == SandboxProviderKind::DOCKER {
        prune::prune(PruneRequest {
            sandbox: SandboxProviderConfig::from_lookup(None, |name| env::var(name).ok()),
            run_id: harness.run_id.to_string(),
            run_dir: harness.run_dir.clone(),
            store: harness.store.clone(),
            provider,
        })
        .await
        .expect("the test container is pruned");
    }
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert_eq!(harness.checkpoints().len(), 4);
    assert_eq!(publisher.pushed.lock().unwrap().len(), 4);
    assert!(!harness.run_dir.join("snapshots").exists());
    assert!(
        !harness
            .workspaces()
            .workspace_exists("invocation-0-scope-0")
            .await
    );
}

/// An upstream repository with `commits` commits on `main`, each changing
/// `README.md`, and the commit `main` ends on.
async fn upstream(root: &Path, commits: usize) -> (PathBuf, String) {
    let work = root.join("upstream-work");
    let bare = root.join("upstream.git");
    fs::create_dir_all(&work)
        .await
        .expect("the work tree creates");
    git(&work, &["init", "-q", "-b", "main"]).await;
    for index in 1..=commits {
        fs::write(work.join("README.md"), format!("revision {index}\n"))
            .await
            .expect("the file writes");
        git(&work, &["add", "README.md"]).await;
        git(&work, &[
            "-c",
            "user.name=Upstream",
            "-c",
            "user.email=upstream@example.com",
            "commit",
            "-q",
            "-m",
            &format!("revision {index}"),
        ])
        .await;
    }
    git(root, &[
        "clone",
        "-q",
        "--bare",
        &work.to_string_lossy(),
        &bare.to_string_lossy(),
    ])
    .await;
    let head = git(&work, &["rev-parse", "HEAD"]).await;
    (bare, head)
}

/// A Git target's run starts from its repository at depth one: the stage
/// sees the files and a shallow history, the snapshot repository is seeded
/// with the starting commit, and every checkpoint builds on it (a commit the
/// stage made itself included), whether the workspace is on the host or
/// `git` runs through the scope's environment and checkpoints leave as
/// bundles (which a shallow clone could not send whole).
#[tokio::test]
async fn a_git_source_is_checked_out_shallow_and_checkpoints_build_on_its_commit() {
    for sandboxed in [false, true] {
        let mut harness = Harness::new().await;
        let (origin, head) = upstream(&harness.run_dir.with_file_name("upstream"), 3).await;
        harness.sandboxed = sandboxed;
        harness.source = Some(file_source(&origin, "main", Some(1)));
        let workflow = workflow(
            "  edit [shape=parallelogram, script=\"test \\\"$(cat README.md)\\\" = 'revision 3'              && test \\\"$(git rev-parse --is-shallow-repository)\\\" = true && git rev-parse              origin/main && echo edited >> README.md && git -c user.name=Agent -c \
             user.email=agent@example.com commit -q -am 'agent edit' && echo uncommitted > \
             notes.txt\"]",
            "  start -> edit -> exit",
        );
        let outcome = harness
            .run_on(SandboxProviderKind::LOCAL, &workflow, SETTINGS)
            .await;
        assert_eq!(
            outcome.status,
            RunStatus::Success,
            "sandboxed={sandboxed}: {outcome:?}"
        );

        let workspace = harness.workspace().await;
        let workspaces = harness.workspaces();
        assert!(!harness.run_dir.join("snapshots").exists());
        let repository = workspaces.workspace_path(&workspace);
        let checkpoints = harness.checkpoints();
        let (_, last) = checkpoints.last().expect("a checkpoint was recorded");
        assert_eq!(
            git(&repository, &["show", &format!("{last}:README.md")]).await,
            "revision 3\nedited",
            "sandboxed={sandboxed}: the last checkpoint carries the stage's edit"
        );
        let (_, first) = checkpoints.first().expect("a checkpoint was recorded");
        assert_eq!(
            git(&repository, &["rev-parse", &format!("{first}^")]).await,
            head,
            "sandboxed={sandboxed}: the run branch starts on the source's commit"
        );
        assert_eq!(
            git(&repository, &["show", &format!("{last}:notes.txt")]).await,
            "uncommitted",
            "sandboxed={sandboxed}: the checkpoint after the stage's own commit carries the rest"
        );
        assert_eq!(
            git(&repository, &["rev-list", "--count", last]).await,
            (checkpoints.len() + 2).to_string(),
            "sandboxed={sandboxed}: the snapshot holds the run's commits, the stage's own commit \
             among them, on the one starting commit"
        );
    }
}

/// A workspace that already holds a repository is not checked out again:
/// the source is fetched once per fresh workspace.
#[tokio::test]
async fn a_prepared_workspace_is_left_as_it_is() {
    let root = tempfile::tempdir().expect("a temp dir");
    let (origin, _) = upstream(root.path(), 1).await;
    let workspaces = RunWorkspaces::new(
        root.path().join("run"),
        "run-1".to_string(),
        GitAuthor::default(),
        &RunCheckpointSettings::default(),
    )
    .with_source(Some(file_source(&origin, "main", None)));
    let path = root.path().join("prepared");
    fs::create_dir_all(&path)
        .await
        .expect("the workspace creates");
    git(&path, &["init", "-q"]).await;
    let site = Site::Host(path.clone());
    assert_eq!(
        workspaces
            .check_out_source(&site, "prepared")
            .await
            .expect("the check succeeds"),
        None
    );
    assert!(!path.join("README.md").exists());
}

/// A revision the origin does not have fails the checkout with git's reason.
#[tokio::test]
async fn an_unavailable_revision_fails_the_checkout() {
    let root = tempfile::tempdir().expect("a temp dir");
    let (origin, _) = upstream(root.path(), 1).await;
    let workspaces = RunWorkspaces::new(
        root.path().join("run"),
        "run-1".to_string(),
        GitAuthor::default(),
        &RunCheckpointSettings::default(),
    )
    .with_source(Some(file_source(&origin, "missing", Some(1))));
    let path = root.path().join("fresh");
    let site = Site::Host(path);
    let error = workspaces
        .check_out_source(&site, "fresh")
        .await
        .expect_err("the branch does not exist");
    assert!(error.to_string().contains("git fetch failed"), "{error}");
}

/// A publisher that records what it was handed and answers as told.
struct RecordingPublisher {
    pushed:    std::sync::Mutex<Vec<(Site, String, String)>>,
    published: std::sync::Mutex<Vec<Publication>>,
    fail:      Option<String>,
}

impl RecordingPublisher {
    fn new(fail: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            pushed:    std::sync::Mutex::default(),
            published: std::sync::Mutex::default(),
            fail:      fail.map(str::to_string),
        })
    }
}

/// The source of a run checked out from the local repository `origin`.
fn file_source(origin: &Path, branch: &str, depth: Option<u32>) -> RunSource {
    RunSource {
        origin: format!("file://{}", origin.display()),
        revision: SourceRevision::Branch(branch.to_string()),
        branch: branch.to_string(),
        depth,
        credentials: None,
    }
}

#[async_trait::async_trait]
impl RunPublisher for RecordingPublisher {
    async fn push(&self, site: &Site, branch: &str, sha: &str) -> Result<(), String> {
        self.pushed
            .lock()
            .unwrap()
            .push((site.clone(), branch.to_string(), sha.to_string()));
        Ok(())
    }
    async fn publish(&self, publication: &Publication) -> Result<(), String> {
        self.published
            .lock()
            .expect("the ledger locks")
            .push(publication.clone());
        self.fail.clone().map_or(Ok(()), Err)
    }
}

/// A run checked out from `origin` on the local provider, whose one stage
/// has the node attributes `attributes`, published through `publisher`.
async fn published_run(
    attributes: &str,
    publisher: &Arc<RecordingPublisher>,
) -> (Harness, engine::RunOutcome) {
    let mut harness = Harness::new().await;
    let (origin, _) = upstream(&harness.run_dir.with_file_name("upstream"), 2).await;
    harness.source = Some(file_source(&origin, "main", Some(1)));
    harness.publisher = Some(Arc::clone(publisher) as Arc<dyn RunPublisher>);
    let workflow = workflow(
        &format!("  edit [shape=parallelogram, {attributes}]"),
        "  start -> edit -> exit",
    );
    let outcome = harness
        .run_on(SandboxProviderKind::LOCAL, &workflow, SETTINGS)
        .await;
    (harness, outcome)
}

/// A successful run hands its publisher the run branch, the commit it ends
/// on (held by the snapshot repository) and its patch, before the run ends.
#[tokio::test]
async fn a_successful_run_is_published_with_its_branch_head_and_patch() {
    let publisher = RecordingPublisher::new(None);
    let (harness, outcome) = published_run("script=\"echo edited >> README.md\"", &publisher).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(!outcome.publish_failed);

    let published = publisher.published.lock().unwrap().clone();
    assert_eq!(published.len(), 1, "published once");
    let publication = &published[0];
    assert_eq!(
        publication.run_branch,
        format!("fabro/run/{}", harness.run_id)
    );
    let (_, last) = harness.checkpoints().last().cloned().expect("a checkpoint");
    assert_eq!(publication.head_sha, last);
    assert_eq!(
        git(&harness.workspace_path(&harness.workspace().await), &[
            "cat-file", "-t", &last
        ])
        .await,
        "commit"
    );
    assert!(
        publication.patch.contains("+edited"),
        "{}",
        publication.patch
    );
}

/// A publication that fails fails the run, with the reason.
#[tokio::test]
async fn a_failed_publication_fails_the_run() {
    let publisher = RecordingPublisher::new(Some("the push was rejected"));
    let (harness, outcome) = published_run("script=\"echo edited >> README.md\"", &publisher).await;
    assert_eq!(outcome.status, RunStatus::Failed, "{outcome:?}");
    assert!(outcome.publish_failed);
    assert_eq!(outcome.failure.as_deref(), Some("the push was rejected"));
    let inspection = harness.inspection().await;
    assert_eq!(inspection.status.as_deref(), Some("failed"));
    let failure = inspection.finalization_failure.expect("durable failure");
    assert_eq!(failure.code, "publish_failed");
    assert_eq!(failure.message, "the push was rejected");
    let stored = engine::outcome_of(&*harness.store, &harness.run_id.to_string())
        .await
        .unwrap();
    assert_eq!(stored.status, outcome.status);
    assert_eq!(stored.failure, outcome.failure);
    assert!(stored.publish_failed);
    let resumed = harness
        .execute_on(SandboxProviderKind::LOCAL, "", SETTINGS, true)
        .await;
    assert_eq!(resumed.status, outcome.status);
    assert_eq!(resumed.failure, outcome.failure);
    assert_eq!(
        publisher.published.lock().unwrap().len(),
        1,
        "committed resume does not publish twice"
    );
}

/// A run that fails (here, at a goal gate) is not published.
#[tokio::test]
async fn a_failed_run_is_not_published() {
    let publisher = RecordingPublisher::new(None);
    let (_, outcome) = published_run("script=\"exit 3\", goal_gate=true", &publisher).await;
    assert_eq!(outcome.status, RunStatus::Failed, "{outcome:?}");
    assert!(!outcome.publish_failed);
    assert!(publisher.published.lock().unwrap().is_empty());
}

struct OriginPublisher {
    origin: String,
    pushed: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl RunPublisher for OriginPublisher {
    async fn push(&self, site: &Site, branch: &str, sha: &str) -> Result<(), String> {
        assert!(
            matches!(site, Site::Sandbox(_)),
            "push runs through the sandbox environment"
        );
        site.push(
            &self.origin,
            &format!("{sha}:refs/heads/{branch}"),
            &[],
            std::time::Duration::from_secs(30),
        )
        .await
        .map_err(|error| error.to_string())?;
        self.pushed.lock().unwrap().push(sha.to_owned());
        Ok(())
    }

    async fn publish(&self, publication: &Publication) -> Result<(), String> {
        self.push(
            &publication.site,
            &publication.run_branch,
            &publication.head_sha,
        )
        .await
    }
}

/// The shallow checkout regression: a fork fetches a published checkpoint
/// directly from the origin inside its new sandbox, after the old workspace
/// has gone. No server Git refs or bundles participate.
#[tokio::test]
async fn a_shallow_run_pushes_each_checkpoint_and_its_fork_fetches_from_the_origin() {
    assert_shallow_fork(1).await;
}

#[tokio::test]
async fn a_terminal_checkpoint_is_refused_before_creating_a_fork() {
    assert_shallow_fork(3).await;
}

async fn assert_shallow_fork(checkpoint_index: usize) {
    use fabro_petri::fork::{self, ForkRequest};
    let mut original = Harness::new().await;
    let (origin, _) = upstream(&original.run_dir.with_file_name("remote"), 5).await;
    original.source = Some(file_source(&origin, "main", Some(1)));
    original.sandboxed = true;
    let publisher = Arc::new(OriginPublisher {
        origin: format!("file://{}", origin.display()),
        pushed: std::sync::Mutex::default(),
    });
    original.publisher = Some(publisher.clone());
    let graph = workflow(
        r#"  write [shape=parallelogram, script="printf 'durable bytes\nsecond line\n' > result.txt"]
  verify [shape=parallelogram, script="test -f result.txt && test -f README.md && cat result.txt"]"#,
        "  start -> write -> verify -> exit",
    );
    let outcome = original.run(&graph, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let checkpoints = original.checkpoints();
    let pushed = publisher.pushed.lock().unwrap().clone();
    assert_eq!(
        &pushed[..checkpoints.len()],
        checkpoints
            .iter()
            .map(|(_, sha)| sha.clone())
            .collect::<Vec<_>>()
    );
    assert!(!original.run_dir.join("snapshots").exists());

    let mut forked = Harness::new().await;
    forked.store = original.store.clone();
    forked.records = original.records.clone();
    forked.source = original.source.clone();
    forked.sandboxed = true;
    forked.publisher = Some(publisher.clone());
    let (key, checkpoint_sha) = &checkpoints[checkpoint_index];
    if checkpoint_index == checkpoints.len() - 1 {
        let refused = fork::check(
            original.store.as_ref(),
            original.run_id,
            fork::position(key.execution, key.firing),
        )
        .await
        .expect_err("terminal checkpoint is refused");
        assert!(
            refused
                .to_string()
                .contains("terminal checkpoint has no remaining work")
        );
        assert!(
            !forked.run_dir.exists(),
            "refuse before creating a new run or sandbox"
        );
        return;
    }
    let seeded = fork::fork(ForkRequest {
        source:       original.run_id,
        fork:         forked.run_id,
        fork_run_dir: forked.run_dir.clone(),
        store:        forked.store.clone(),
        records:      forked.records.clone(),
        position:     fork::position(key.execution, key.firing),
        rerun_last:   false,
        settings:     RunNamespace::default(),
    })
    .await
    .expect("the fork is seeded");
    assert_eq!(
        seeded.start.expect("the selected checkpoint").sha,
        *checkpoint_sha
    );
    fs::remove_dir_all(&original.run_dir)
        .await
        .expect("the original workspace is deleted");
    let outcome = forked
        .execute_on(SandboxProviderKind::LOCAL, &graph, SETTINGS, true)
        .await;
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let path = forked.workspace_path(&forked.workspace().await);
    if let Some((_, first_new)) = forked.checkpoints().get(checkpoint_index + 1) {
        assert_eq!(
            git(&path, &["rev-parse", &format!("{first_new}^")]).await,
            *checkpoint_sha,
            "the fork continues from the selected checkpoint, not the source run's final head"
        );
    } else {
        assert_eq!(git(&path, &["rev-parse", "HEAD"]).await, *checkpoint_sha);
    }
    assert_eq!(
        fs::read(path.join("result.txt"))
            .await
            .expect("the recovered file reads"),
        b"durable bytes\nsecond line\n"
    );
    assert_eq!(
        git(&path, &["branch", "--show-current"]).await,
        format!("fabro/run/{}", forked.run_id)
    );
    assert_eq!(
        git(&origin, &[
            "rev-parse",
            &format!("refs/heads/fabro/run/{}", forked.run_id)
        ])
        .await,
        git(&path, &["rev-parse", "HEAD"]).await
    );
    assert!(!forked.run_dir.join("snapshots").exists());
}

/// A host workspace with no repository is initialized and checkpointed: every
/// stage commits on the run branch, and nothing is kept on the server.
#[tokio::test]
async fn an_empty_host_workspace_is_initialized_and_checkpointed() {
    let mut harness = Harness::new().await;
    harness.source = None;
    let graph = workflow(
        r#"  write [shape=parallelogram, script="echo data > result.txt"]"#,
        "  start -> write -> exit",
    );
    let outcome = harness.run(&graph, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Success);
    assert_eq!(harness.checkpoints().len(), 3);
    let path = harness.workspace_path(&harness.workspace().await);
    assert_eq!(commits(&path).await.len(), 3);
    assert_eq!(
        git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).await,
        format!("fabro/run/{}", harness.run_id)
    );
    assert!(!harness.run_dir.join("snapshots").exists());
}

#[tokio::test]
async fn ordinary_recovery_refuses_a_missing_workspace() {
    use fabro_petri::recovery::RestoreTarget;
    let harness = Harness::new().await;
    let graph = workflow(
        r#"  write [shape=parallelogram, script="echo data > result.txt"]"#,
        "  start -> write -> exit",
    );
    assert_eq!(
        harness.run(&graph, SETTINGS).await.status,
        RunStatus::Success
    );
    let workspace = harness.workspace().await;
    let path = harness.workspace_path(&workspace);
    let (key, sha) = harness.checkpoints().last().unwrap().clone();
    fs::remove_dir_all(&path).await.unwrap();
    let result = recovery::bring_to(
        &harness.workspaces(),
        &Site::Host(path.clone()),
        &workspace,
        &RestoreTarget { key, sha },
    )
    .await;
    assert!(result.is_err());
    assert!(
        !path.exists(),
        "normal resume does not recreate the checkout"
    );
}

struct RejectRunDiff(Arc<MemoryPlatformRecords>);

#[async_trait::async_trait]
impl PlatformRecords for RejectRunDiff {
    async fn append(
        &self,
        run_id: &RunId,
        record: &PlatformRecord,
        position: Option<fabro_store::StagePosition>,
    ) -> Result<fabro_store::StoredPlatformRecord, PlatformRecordError> {
        if matches!(record, PlatformRecord::RunDiff(_)) {
            return Err(PlatformRecordError::Store(fabro_store::Error::Io(
                std::io::Error::other("run diff unavailable"),
            )));
        }
        self.0.append(run_id, record, position).await
    }
    async fn read_kind(
        &self,
        run_id: &RunId,
        kind: PlatformRecordKind,
    ) -> Result<Vec<fabro_store::StoredPlatformRecord>, PlatformRecordError> {
        self.0.read_kind(run_id, kind).await
    }
}

#[tokio::test]
async fn a_run_diff_failure_cannot_silently_skip_publication() {
    let mut harness = Harness::new().await;
    harness.fail_run_diff = true;
    let publisher = RecordingPublisher::new(None);
    harness.publisher = Some(publisher.clone());
    let graph = workflow(
        r#"  write [shape=parallelogram, script="echo data > result.txt"]"#,
        "  start -> write -> exit",
    );
    let outcome = harness.run(&graph, SETTINGS).await;
    assert_eq!(outcome.status, RunStatus::Failed, "{outcome:?}");
    assert!(outcome.publish_failed);
    assert!(
        outcome
            .failure
            .as_ref()
            .unwrap()
            .contains("run diff unavailable")
    );
    let stored = engine::outcome_of(&*harness.store, &harness.run_id.to_string())
        .await
        .unwrap();
    assert_eq!(stored.status, outcome.status);
    assert_eq!(stored.failure, outcome.failure);
    assert!(stored.publish_failed);
    assert!(publisher.published.lock().unwrap().is_empty());
}

struct GatedPublisher {
    inner:   Arc<RecordingPublisher>,
    entered: Notify,
    release: Semaphore,
}

#[async_trait::async_trait]
impl RunPublisher for GatedPublisher {
    async fn push(&self, site: &Site, branch: &str, sha: &str) -> Result<(), String> {
        self.inner.push(site, branch, sha).await
    }

    async fn publish(&self, publication: &Publication) -> Result<(), String> {
        self.entered.notify_one();
        self.release
            .acquire()
            .await
            .expect("publication gate stays open")
            .forget();
        self.inner.publish(publication).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_publication_blocks_the_terminal_result_and_cleanup() {
    for rejection in [None, Some("the push was rejected")] {
        let publisher = Arc::new(GatedPublisher {
            inner:   RecordingPublisher::new(rejection),
            entered: Notify::new(),
            release: Semaphore::new(0),
        });
        let mut harness = Harness::new().await;
        harness.publisher = Some(publisher.clone());
        let harness = Arc::new(harness);
        let executing = harness.clone();
        let task = tokio::spawn(async move {
            executing
                .run(
                    &workflow(
                        "edit [shape=parallelogram, script=\"echo edited >> README.md\"]",
                        "start -> edit -> exit",
                    ),
                    SETTINGS,
                )
                .await
        });
        time::timeout(
            std::time::Duration::from_secs(15),
            publisher.entered.notified(),
        )
        .await
        .expect("publication reaches the gate");
        let pending = harness.inspection().await;
        assert!(pending.required_finalization);
        assert!(
            pending.status.is_none(),
            "no terminal result while publication waits"
        );
        assert!(!task.is_finished());
        let logs = harness
            .store
            .open(&RunKey::new(harness.run_id.to_string()), Access::Read)
            .await
            .unwrap();
        let coordinator = logs.read(&petri_store::LogId::Coordinator).await.unwrap();
        assert!(
            coordinator
                .iter()
                .all(|record| record.record["body"]["event"] != "scope.released"),
            "scope cleanup waits for publication"
        );
        assert!(
            harness.workspace_path(&harness.workspace().await).exists(),
            "workspace is available to publication"
        );
        assert!(matches!(
            engine::outcome_of(&*harness.store, &harness.run_id.to_string()).await,
            Err(engine::RunError::Unfinished(_))
        ));
        publisher.release.add_permits(1);
        let outcome = task.await.unwrap();
        assert_eq!(
            outcome.status,
            if rejection.is_some() {
                RunStatus::Failed
            } else {
                RunStatus::Success
            }
        );
        assert_eq!(outcome.publish_failed, rejection.is_some());
        let stored = engine::outcome_of(&*harness.store, &harness.run_id.to_string())
            .await
            .unwrap();
        assert_eq!(stored.status, outcome.status);
        assert_eq!(stored.failure, outcome.failure);
        assert_eq!(publisher.inner.published.lock().unwrap().len(), 1);
    }
}
