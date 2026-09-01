use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use fabro_config::RunLayer;
use fabro_manifest::{
    CollectedWorkflowClosure, ResolvedLocalWorkflowPackage, RunOverrideInput,
    collect_workflow_versions, observe_git_run_target, resolve_local_workflow_package,
};
use fabro_tool::{
    PreparedRunCreate, RunCreateAdapter, ValidatedCreateRunSpec, ValidatedCreateRunWorkflowSource,
};
use fabro_types::settings::run::EnvironmentProvider;
use fabro_types::{DirtyStatus, RunTarget};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::manifest_validation;

#[derive(Clone, Debug)]
pub struct ServerRunCreateAdapter {
    mode: RunCreateMode,
}

#[derive(Clone, Debug)]
enum RunCreateMode {
    Standalone {
        user_workflows_root: Option<PathBuf>,
    },
    Worker {
        provider:            EnvironmentProvider,
        inherited_target:    Option<RunTarget>,
        user_workflows_root: Option<PathBuf>,
    },
}

impl ServerRunCreateAdapter {
    #[must_use]
    pub fn standalone(user_workflows_root: Option<PathBuf>) -> Self {
        Self {
            mode: RunCreateMode::Standalone {
                user_workflows_root,
            },
        }
    }

    #[must_use]
    pub fn worker(
        provider: EnvironmentProvider,
        inherited_target: Option<RunTarget>,
        user_workflows_root: Option<PathBuf>,
    ) -> Self {
        Self {
            mode: RunCreateMode::Worker {
                provider,
                inherited_target,
                user_workflows_root,
            },
        }
    }

    fn has_shared_filesystem(&self) -> bool {
        match self.mode {
            RunCreateMode::Standalone { .. } => true,
            RunCreateMode::Worker { provider, .. } => provider.is_local(),
        }
    }

    fn user_workflows_root(&self) -> Option<&Path> {
        match &self.mode {
            RunCreateMode::Standalone {
                user_workflows_root,
            }
            | RunCreateMode::Worker {
                user_workflows_root,
                ..
            } => user_workflows_root.as_deref(),
        }
    }

    async fn resolve_goal(
        &self,
        spec: &ValidatedCreateRunSpec,
        cwd: &Path,
    ) -> Result<Option<String>> {
        if let Some(goal) = &spec.goal {
            return Ok(Some(goal.clone()));
        }
        let Some(goal_file) = &spec.goal_file else {
            return Ok(None);
        };
        if !self.has_shared_filesystem() {
            bail!(
                "goal_file requires a shared Local filesystem; Docker and Daytona callers must send goal text by value"
            );
        }
        let path = cwd.join(goal_file);
        fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read goal file {}", path.display()))
            .map(Some)
    }

    fn resolve_target(&self, spec: &ValidatedCreateRunSpec, cwd: &Path) -> Result<ResolvedTarget> {
        if let Some(target) = &spec.target {
            return Ok(ResolvedTarget {
                target:   target.clone(),
                warnings: Vec::new(),
            });
        }

        match &self.mode {
            RunCreateMode::Worker {
                inherited_target: Some(target),
                ..
            } => Ok(ResolvedTarget {
                target:   target.clone(),
                warnings: Vec::new(),
            }),
            RunCreateMode::Worker {
                inherited_target: None,
                ..
            } => bail!(
                "the parent run has no canonical target; send an explicit target for this child run"
            ),
            RunCreateMode::Standalone { .. } => {
                let observation = observe_git_run_target(cwd, None).ok_or_else(|| {
                    anyhow::anyhow!(
                        "target is required outside an attached local GitHub checkout with a branch"
                    )
                })?;
                let target = observation.run_target.ok_or_else(|| {
                    anyhow::anyhow!(
                        "target is required because the local checkout cannot be represented as a GitHub run target"
                    )
                })?;
                let mut warnings = Vec::new();
                if observation.legacy_git_context.dirty == DirtyStatus::Dirty {
                    warnings.push(
                        "the local checkout has uncommitted changes; those changes are excluded from the run target"
                            .to_string(),
                    );
                }
                if observation
                    .legacy_git_context
                    .sha
                    .as_deref()
                    .is_some_and(|sha| !sha.is_empty())
                    && target.sha.is_none()
                {
                    warnings.push(
                        "the local HEAD commit is not fetchable and is not pinned; the remote branch will be selected"
                            .to_string(),
                    );
                }
                Ok(ResolvedTarget {
                    target: RunTarget::Git(target),
                    warnings,
                })
            }
        }
    }

    fn collect_selector(&self, selector: &str, cwd: &Path) -> Result<LocalWorkflowSource> {
        if !self.has_shared_filesystem() {
            bail!(
                "workflow selectors require a shared Local filesystem; send inline files or an exact stored workflow version ID from Docker or Daytona"
            );
        }
        resolve_local_workflow_package(Path::new(selector), cwd, self.user_workflows_root())
            .map(LocalWorkflowSource::Selector)
            .map_err(anyhow::Error::new)
    }
}

#[async_trait]
impl RunCreateAdapter for ServerRunCreateAdapter {
    async fn prepare(
        &self,
        client: &fabro_client::Client,
        spec: &ValidatedCreateRunSpec,
        cwd: &Path,
    ) -> Result<PreparedRunCreate> {
        if !self.has_shared_filesystem()
            && matches!(spec.workflow, ValidatedCreateRunWorkflowSource::Selector(_))
        {
            bail!(
                "workflow selectors require a shared Local filesystem; send inline files or an exact stored workflow version ID from Docker or Daytona"
            );
        }
        if !self.has_shared_filesystem() && spec.goal_file.is_some() {
            bail!(
                "goal_file requires a shared Local filesystem; Docker and Daytona callers must send goal text by value"
            );
        }

        let goal = self.resolve_goal(spec, cwd).await?;
        if let ValidatedCreateRunWorkflowSource::Stored {
            workflow_version_id,
        } = spec.workflow
        {
            let resolved_target = self.resolve_target(spec, cwd)?;
            return Ok(PreparedRunCreate {
                workflow_version_id,
                target: resolved_target.target,
                goal,
                warnings: resolved_target.warnings,
            });
        }

        let local_source = match &spec.workflow {
            ValidatedCreateRunWorkflowSource::Selector(selector) => {
                self.collect_selector(selector, cwd)?
            }
            ValidatedCreateRunWorkflowSource::Inline(source) => {
                LocalWorkflowSource::inline(source).await?
            }
            ValidatedCreateRunWorkflowSource::Stored { .. } => unreachable!(),
        };
        validate_local_source(local_source.closure(), spec, goal.as_deref())?;
        let resolved_target = self.resolve_target(spec, cwd)?;
        let closure = local_source.closure();
        let versions = closure
            .versions()
            .map(|(_, version)| version.version())
            .collect::<Vec<_>>();
        client.register_workflow_versions(versions).await?;

        Ok(PreparedRunCreate {
            workflow_version_id: closure.root_id(),
            target: resolved_target.target,
            goal,
            warnings: resolved_target.warnings,
        })
    }
}

struct ResolvedTarget {
    target:   RunTarget,
    warnings: Vec<String>,
}

enum LocalWorkflowSource {
    Selector(ResolvedLocalWorkflowPackage),
    Inline {
        closure: CollectedWorkflowClosure,
        _root:   tempfile::TempDir,
    },
}

impl LocalWorkflowSource {
    async fn inline(source: &fabro_tool::InlineWorkflowSource) -> Result<Self> {
        let root = tempfile::tempdir().context("failed to create private inline workflow root")?;
        for (path, content) in &source.files {
            let destination = root.path().join(path.as_str());
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).await.with_context(|| {
                    format!(
                        "failed to create inline workflow directory {}",
                        parent.display()
                    )
                })?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .await
                .with_context(|| {
                    format!(
                        "failed to create inline workflow file {}",
                        destination.display()
                    )
                })?;
            file.write_all(content.as_bytes()).await.with_context(|| {
                format!(
                    "failed to write inline workflow file {}",
                    destination.display()
                )
            })?;
        }
        let closure = collect_workflow_versions(Path::new(source.entrypoint.as_str()), root.path())
            .map_err(anyhow::Error::new)?;
        Ok(Self::Inline {
            closure,
            _root: root,
        })
    }

    fn closure(&self) -> &CollectedWorkflowClosure {
        match self {
            Self::Selector(package) => package.closure(),
            Self::Inline { closure, .. } => closure,
        }
    }
}

fn validate_local_source(
    closure: &CollectedWorkflowClosure,
    spec: &ValidatedCreateRunSpec,
    goal: Option<&str>,
) -> Result<()> {
    let run_overrides = run_tool_run_overrides(spec, goal);
    let inputs = spec
        .inputs
        .iter()
        .map(|(key, value)| (key.clone(), value.toml().clone()))
        .collect::<HashMap<_, _>>();
    let response =
        manifest_validation::validate_collected_workflow(closure, run_overrides.as_ref(), &inputs)?;
    if !response.ok {
        bail!("workflow validation failed");
    }
    Ok(())
}

fn run_tool_run_overrides(spec: &ValidatedCreateRunSpec, goal: Option<&str>) -> Option<RunLayer> {
    fabro_manifest::build_sparse_run_overrides(RunOverrideInput {
        goal,
        model: spec.model.as_deref(),
        provider: spec.provider.as_deref(),
        environment: spec.environment.as_deref(),
        preserve_sandbox: spec.preserve_sandbox,
        dry_run: spec.dry_run,
        auto_approve: spec.auto_approve,
        labels: spec.labels.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use fabro_tool::{FabroRunCreateParams, ValidatedCreateRuns};
    use fabro_types::{GitRunTarget, WorkflowVersion, WorkflowVersionId};
    use httpmock::Method::POST;
    use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
    use serde_json::json;

    use super::*;

    fn validated_spec(value: &serde_json::Value) -> ValidatedCreateRunSpec {
        let params: FabroRunCreateParams = serde_json::from_value(json!({ "runs": [value] }))
            .expect("create input should deserialize");
        ValidatedCreateRuns::try_from(params)
            .expect("create input should validate")
            .runs
            .remove(0)
    }

    fn no_proxy_client(base_url: &str) -> fabro_client::Client {
        fabro_client::Client::new_no_proxy(base_url).expect("test client should build")
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test fixture setup uses the Git CLI against an isolated temporary repository"
    )]
    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn dynamic_version_registration_mock(
        server: &MockServer,
        registered: Arc<Mutex<Vec<WorkflowVersion>>>,
    ) -> httpmock::Mock<'_> {
        server
            .mock_async(move |when, then| {
                when.method(POST).path("/api/v1/workflow-versions");
                then.respond_with(move |request: &HttpMockRequest| {
                    let version: WorkflowVersion = serde_json::from_str(&request.body_string())
                        .expect("registration request should contain a workflow version");
                    let id = version.id().expect("registered version should have an ID");
                    registered.lock().unwrap().push(version);
                    HttpMockResponse::builder()
                        .status(201)
                        .header("content-type", "application/json")
                        .body(json!({ "workflow_version_id": id }).to_string())
                        .build()
                });
            })
            .await
    }

    #[tokio::test]
    async fn workflow_version_inline_create_registers_exact_dependency_first_bytes() {
        let server = MockServer::start_async().await;
        let registered = Arc::new(Mutex::new(Vec::new()));
        let registration =
            dynamic_version_registration_mock(&server, Arc::clone(&registered)).await;
        let client = no_proxy_client(&server.url(""));
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "inline",
                "entrypoint": "root/workflow.fabro",
                "files": {
                    "root/workflow.fabro": r#"digraph Root {
                        start [shape=Mdiamond]
                        prompt [prompt="@prompt.md"]
                        child [stack.child_workflow="../child/workflow.fabro"]
                        exit [shape=Msquare]
                        start -> prompt -> child -> exit
                    }"#,
                    "root/prompt.md": "runtime-authored root bytes",
                    "child/workflow.fabro": r#"digraph Child {
                        start [shape=Mdiamond]
                        task [prompt="@support.md"]
                        exit [shape=Msquare]
                        start -> task -> exit
                    }"#,
                    "child/support.md": "runtime-authored child bytes"
                }
            },
            "target": { "kind": "none" },
            "start": false
        }));
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Docker, None, None);

        let prepared = adapter
            .prepare(&client, &spec, Path::new("/host/that-must-not-be-read"))
            .await
            .expect("inline workflow should prepare");

        registration.assert_calls_async(2).await;
        let registered = registered.lock().unwrap();
        assert_eq!(registered.len(), 2);
        let child_id = registered[0].id().unwrap();
        let root_id = registered[1].id().unwrap();
        assert_eq!(prepared.workflow_version_id, root_id);
        assert_eq!(prepared.target, RunTarget::None {});
        assert_eq!(
            registered[1].workflow_dependencies(),
            &BTreeMap::from([(
                fabro_types::WorkflowPath::new("child/workflow.fabro").unwrap(),
                child_id,
            )])
        );
        assert_eq!(
            registered[0]
                .files()
                .get(&fabro_types::WorkflowPath::new("child/support.md").unwrap())
                .map(String::as_str),
            Some("runtime-authored child bytes")
        );
        assert_eq!(
            registered[1]
                .files()
                .get(&fabro_types::WorkflowPath::new("root/prompt.md").unwrap())
                .map(String::as_str),
            Some("runtime-authored root bytes")
        );
    }

    #[tokio::test]
    async fn workflow_version_stored_create_skips_registration_and_inherits_exact_worker_target() {
        let client = no_proxy_client("http://127.0.0.1:9");
        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let inherited = RunTarget::Git(GitRunTarget {
            repo:   "fabro-sh/fabro".to_string(),
            branch: "main".to_string(),
            tag:    Some("v1.0.0".to_string()),
            sha:    Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        });
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            }
        }));
        let adapter = ServerRunCreateAdapter::worker(
            EnvironmentProvider::Docker,
            Some(inherited.clone()),
            None,
        );

        let prepared = adapter
            .prepare(&client, &spec, Path::new("/ignored"))
            .await
            .unwrap();

        assert_eq!(prepared.workflow_version_id, workflow_version_id);
        assert_eq!(prepared.target, inherited);
    }

    #[tokio::test]
    async fn workflow_version_selector_uses_resolved_package_root_not_operation_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let operation_cwd = temp.path().join("nested/operation");
        let workflow_dir = temp.path().join(".fabro/workflows/demo");
        fs::create_dir_all(&operation_cwd).await.unwrap();
        fs::create_dir_all(&workflow_dir).await.unwrap();
        fs::write(temp.path().join(".fabro/project.toml"), "_version = 1\n")
            .await
            .unwrap();
        fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .await
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.fabro"),
            "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .await
        .unwrap();
        let server = MockServer::start_async().await;
        let registered = Arc::new(Mutex::new(Vec::new()));
        let registration =
            dynamic_version_registration_mock(&server, Arc::clone(&registered)).await;
        let client = no_proxy_client(&server.url(""));
        let spec = validated_spec(&json!({
            "workflow": "demo",
            "target": { "kind": "none" }
        }));
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Local, None, None);

        let prepared = adapter
            .prepare(&client, &spec, &operation_cwd)
            .await
            .unwrap();

        registration.assert_calls_async(1).await;
        let registered = registered.lock().unwrap();
        assert_eq!(prepared.workflow_version_id, registered[0].id().unwrap());
        assert_eq!(
            registered[0].entrypoint().as_str(),
            ".fabro/workflows/demo/workflow.fabro"
        );
    }

    #[tokio::test]
    async fn workflow_version_worker_capabilities_gate_selector_and_goal_file_before_reads() {
        let temp = tempfile::tempdir().unwrap();
        let workflow = temp.path().join("same-name.fabro");
        fs::write(
            &workflow,
            "digraph HostCopy { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .await
        .unwrap();
        fs::write(
            temp.path().join("goal.md"),
            "host goal that must not be read",
        )
        .await
        .unwrap();
        let client = no_proxy_client("http://127.0.0.1:9");
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Daytona, None, None);

        let selector = validated_spec(&json!({
            "workflow": "same-name.fabro",
            "target": { "kind": "none" }
        }));
        let selector_error = adapter
            .prepare(&client, &selector, temp.path())
            .await
            .expect_err("Daytona worker must reject host selectors");
        assert!(
            selector_error
                .to_string()
                .contains("inline files or an exact stored")
        );

        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let goal_file = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "target": { "kind": "none" },
            "goal_file": "goal.md"
        }));
        let goal_error = adapter
            .prepare(&client, &goal_file, temp.path())
            .await
            .expect_err("Daytona worker must reject host goal files");
        assert!(goal_error.to_string().contains("send goal text by value"));
    }

    #[tokio::test]
    async fn workflow_version_local_content_is_validated_before_registration() {
        let client = no_proxy_client("http://127.0.0.1:9");
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Docker, None, None);

        let invalid_graph = validated_spec(&json!({
            "workflow": {
                "kind": "inline",
                "entrypoint": "workflow.fabro",
                "files": { "workflow.fabro": "this is not a graph" }
            },
            "target": { "kind": "none" }
        }));
        adapter
            .prepare(&client, &invalid_graph, Path::new("/ignored"))
            .await
            .expect_err("invalid graph should fail before registration");

        let undefined_input = validated_spec(&json!({
            "workflow": {
                "kind": "inline",
                "entrypoint": "workflow.fabro",
                "files": {
                    "workflow.fabro": r#"digraph W {
                        start [shape=Mdiamond]
                        task [prompt="@prompt.md"]
                        exit [shape=Msquare]
                        start -> task -> exit
                    }"#,
                    "prompt.md": "Hello {{ inputs.owner }}"
                }
            },
            "target": { "kind": "none" }
        }));
        let error = adapter
            .prepare(&client, &undefined_input, Path::new("/ignored"))
            .await
            .expect_err("undefined input should fail before registration");
        assert!(error.to_string().contains("workflow validation failed"));
    }

    #[tokio::test]
    async fn workflow_version_target_failure_precedes_registration() {
        let client = no_proxy_client("http://127.0.0.1:9");
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Docker, None, None);
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "inline",
                "entrypoint": "workflow.fabro",
                "files": {
                    "workflow.fabro": "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                }
            }
        }));

        let error = adapter
            .prepare(&client, &spec, Path::new("/ignored"))
            .await
            .expect_err("missing inherited target should fail before registration");

        assert!(
            error
                .to_string()
                .contains("parent run has no canonical target"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn workflow_version_shared_goal_file_and_explicit_target_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("goal.md"), "goal from shared filesystem")
            .await
            .unwrap();
        let client = no_proxy_client("http://127.0.0.1:9");
        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "target": { "kind": "none" },
            "goal_file": "goal.md"
        }));
        let inherited = RunTarget::Folder {
            path: "/parent/workspace".to_string(),
        };
        let adapter =
            ServerRunCreateAdapter::worker(EnvironmentProvider::Local, Some(inherited), None);

        let prepared = adapter.prepare(&client, &spec, temp.path()).await.unwrap();

        assert_eq!(prepared.target, RunTarget::None {});
        assert_eq!(
            prepared.goal.as_deref(),
            Some("goal from shared filesystem")
        );
    }

    #[tokio::test]
    async fn workflow_version_standalone_git_fallback_reports_excluded_local_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).await.unwrap();
        run_git(&workspace, &[
            "init",
            "--quiet",
            "--initial-branch",
            "feature",
        ]);
        run_git(&workspace, &["config", "user.name", "Fabro Test"]);
        run_git(&workspace, &["config", "user.email", "fabro@example.com"]);
        fs::write(workspace.join("tracked.txt"), "committed")
            .await
            .unwrap();
        run_git(&workspace, &["add", "tracked.txt"]);
        run_git(&workspace, &["commit", "--quiet", "-m", "initial"]);
        run_git(&workspace, &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ]);
        let missing = format!("file://{}/missing.git", temp.path().display());
        run_git(&workspace, &[
            "remote", "set-url", "--push", "origin", &missing,
        ]);
        fs::write(workspace.join("dirty.txt"), "uncommitted")
            .await
            .unwrap();

        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            }
        }));
        let client = no_proxy_client("http://127.0.0.1:9");
        let adapter = ServerRunCreateAdapter::standalone(None);

        let prepared = adapter.prepare(&client, &spec, &workspace).await.unwrap();

        let RunTarget::Git(target) = prepared.target else {
            panic!("standalone attached Git checkout should derive a Git target");
        };
        assert_eq!(target.repo, "acme/widgets");
        assert_eq!(target.branch, "feature");
        assert_eq!(target.sha, None);
        assert!(
            prepared
                .warnings
                .iter()
                .any(|warning| warning.contains("uncommitted changes"))
        );
        assert!(
            prepared
                .warnings
                .iter()
                .any(|warning| warning.contains("not fetchable") && warning.contains("not pinned"))
        );
    }
}
