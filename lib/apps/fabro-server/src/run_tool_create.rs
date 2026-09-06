use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use fabro_environment::DEFAULT_ENVIRONMENT_ID;
use fabro_manifest::{
    CollectedWorkflowClosure, DerivedRunTarget, collect_inline_workflow_versions,
    configured_repo_origin_url_for_location, configured_repo_origin_url_from_workflow_toml,
    derive_run_target_for_provider, resolve_local_workflow_package,
};
use fabro_tool::{
    CreateRunWorkflowSource, PreparedRunCreate, RunCreateAdapter, ValidatedCreateRunSpec,
};
use fabro_types::RunTarget;
use fabro_types::settings::run::EnvironmentProvider;
use tokio::{fs, task};

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

    async fn resolve_target(
        &self,
        client: &fabro_client::Client,
        spec: &ValidatedCreateRunSpec,
        cwd: &Path,
        configured_repo_origin_url: Option<&str>,
    ) -> Result<ResolvedTarget> {
        if let Some(target) = &spec.target {
            if matches!(target, RunTarget::Folder { .. }) && !self.has_shared_filesystem() {
                bail!(
                    "folder targets require a shared Local filesystem; Docker and Daytona parents cannot select server-host folders"
                );
            }
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
                target:   inherit_parent_target(target),
                warnings: Vec::new(),
            }),
            RunCreateMode::Worker {
                inherited_target: None,
                ..
            } => bail!(
                "the parent run has no canonical target; send an explicit target for this child run"
            ),
            RunCreateMode::Standalone { .. } => {
                // Standalone callers derive the target the same way `fabro run`
                // does: from the selected environment's provider. Local
                // environments run against the caller folder; clone-based
                // environments need a provably published GitHub checkout.
                let environment_id = spec
                    .environment
                    .as_deref()
                    .unwrap_or(DEFAULT_ENVIRONMENT_ID);
                let environment = client
                    .retrieve_environment(environment_id)
                    .await
                    .with_context(|| {
                        format!(
                            "could not retrieve environment `{environment_id}` to derive the run target"
                        )
                    })?;
                let provider = environment.settings.provider;
                let canonical_cwd = fs::canonicalize(cwd).await.with_context(|| {
                    format!("failed to canonicalize run directory {}", cwd.display())
                })?;
                let configured_repo_origin_url = configured_repo_origin_url.map(str::to_owned);
                let DerivedRunTarget {
                    target,
                    dirty_worktree,
                } = task::spawn_blocking(move || {
                    derive_run_target_for_provider(
                        provider,
                        &canonical_cwd,
                        configured_repo_origin_url.as_deref(),
                    )
                })
                .await
                .context("run target derivation task failed")??;
                let mut warnings = Vec::new();
                if dirty_worktree {
                    warnings.push(
                        "the local checkout has uncommitted changes; those changes are excluded from the run target"
                            .to_string(),
                    );
                }
                Ok(ResolvedTarget { target, warnings })
            }
        }
    }

    async fn collect_selector(&self, selector: &str, cwd: &Path) -> Result<CollectedWorkflow> {
        if !self.has_shared_filesystem() {
            bail!(
                "workflow selectors require a shared Local filesystem; send inline files or an exact stored workflow version ID from Docker or Daytona"
            );
        }
        let selector = PathBuf::from(selector);
        let cwd = cwd.to_path_buf();
        let user_workflows_root = self.user_workflows_root().map(Path::to_path_buf);
        task::spawn_blocking(move || {
            let package =
                resolve_local_workflow_package(&selector, &cwd, user_workflows_root.as_deref())?;
            let configured_repo_origin_url =
                configured_repo_origin_url_for_location(package.workflow_location())?;
            Ok(CollectedWorkflow {
                closure: package.into_closure(),
                configured_repo_origin_url,
            })
        })
        .await
        .context("workflow package collection task failed")?
    }
}

/// A packaged workflow closure plus the `run.scm` repository its config names,
/// which standalone target derivation honors over the checkout's own origin.
struct CollectedWorkflow {
    closure:                    CollectedWorkflowClosure,
    configured_repo_origin_url: Option<String>,
}

#[async_trait]
impl RunCreateAdapter for ServerRunCreateAdapter {
    async fn prepare(
        &self,
        client: &fabro_client::Client,
        spec: &ValidatedCreateRunSpec,
        cwd: &Path,
    ) -> Result<PreparedRunCreate> {
        let goal = self.resolve_goal(spec, cwd).await?;
        let CollectedWorkflow {
            closure,
            configured_repo_origin_url,
        } = match &spec.workflow {
            CreateRunWorkflowSource::Stored {
                workflow_version_id,
            } => {
                // A stored version's config is not available locally, so
                // derivation uses the checkout's own origin.
                let resolved_target = self.resolve_target(client, spec, cwd, None).await?;
                return Ok(PreparedRunCreate {
                    workflow_version_id: *workflow_version_id,
                    target: resolved_target.target,
                    goal,
                    warnings: resolved_target.warnings,
                });
            }
            CreateRunWorkflowSource::Selector(selector) => {
                self.collect_selector(selector, cwd).await?
            }
            CreateRunWorkflowSource::Inline(source) => collect_inline_workflow(source).await?,
        };
        let resolved_target = self
            .resolve_target(client, spec, cwd, configured_repo_origin_url.as_deref())
            .await?;
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

/// The target a child inherits when it omits its own. A parent's Git target
/// is pinned to the commit admitted for the parent, and clone-based providers
/// never fall back to branch HEAD, so carrying that pin forward would hide
/// every commit the parent has since pushed from a child meant to review or
/// continue that work. The child follows the parent's branch instead; the
/// pinned commit and tag stay on the parent only. Folder and none targets are
/// inherited as-is.
fn inherit_parent_target(parent: &RunTarget) -> RunTarget {
    match parent {
        RunTarget::Git(git) => RunTarget::Git(fabro_types::GitRunTarget {
            repo:   git.repo.clone(),
            branch: git.branch.clone(),
            tag:    None,
            sha:    None,
        }),
        RunTarget::None {} | RunTarget::Folder { .. } => parent.clone(),
    }
}

/// Collect an inline workflow from its supplied bytes. The entrypoint is an
/// exact key of the file map, never a checkout selector.
async fn collect_inline_workflow(
    source: &fabro_tool::InlineWorkflowSource,
) -> Result<CollectedWorkflow> {
    let entrypoint = source.entrypoint.clone();
    let files = source.files.clone();
    task::spawn_blocking(move || {
        let closure = collect_inline_workflow_versions(&entrypoint, &files)?;
        // The inline config is either the entrypoint itself or the
        // `workflow.toml` beside the entrypoint graph.
        let config_path = if Path::new(entrypoint.as_str())
            .extension()
            .is_some_and(|ext| ext == "toml")
        {
            Some(entrypoint.clone())
        } else {
            entrypoint.resolve_reference("workflow.toml").ok()
        };
        let configured_repo_origin_url = config_path
            .and_then(|path| files.get(&path))
            .map(|source| configured_repo_origin_url_from_workflow_toml(source))
            .transpose()?
            .flatten();
        Ok(CollectedWorkflow {
            closure,
            configured_repo_origin_url,
        })
    })
    .await
    .context("inline workflow package collection task failed")?
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use fabro_tool::fabro_client::ClientBackend;
    use fabro_tool::{FabroRunCreateParams, FabroToolBackend as _, ValidatedCreateRuns};
    use fabro_types::{GitRunTarget, WorkflowVersion, WorkflowVersionId};
    use httpmock::Method::{GET, POST};
    use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
    use serde_json::json;

    use super::*;

    /// Canonical `GET /api/v1/environments/{id}` body for mock servers.
    fn environment_json(id: &str, provider: &str) -> serde_json::Value {
        json!({
            "id": id,
            "revision": "0".repeat(64),
            "provider": provider,
            "image": { "docker": null, "dockerfile": null },
            "resources": { "cpu": null, "memory": null, "disk": null },
            "network": { "mode": "allow_all", "allow": [] },
            "lifecycle": {
                "preserve": false,
                "stop_on_terminal": true,
                "auto_stop": null
            },
            "labels": {},
            "env": {}
        })
    }

    async fn mock_environment<'a>(
        server: &'a MockServer,
        id: &str,
        provider: &str,
    ) -> httpmock::Mock<'a> {
        let path = format!("/api/v1/environments/{id}");
        let body = environment_json(id, provider);
        server
            .mock_async(move |when, then| {
                when.method(GET).path(path);
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(body);
            })
            .await
    }

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
    async fn workflow_version_inline_entrypoint_is_exact_even_without_an_extension() {
        let server = MockServer::start_async().await;
        let registered = Arc::new(Mutex::new(Vec::new()));
        let registration =
            dynamic_version_registration_mock(&server, Arc::clone(&registered)).await;
        let client = no_proxy_client(&server.url(""));
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "inline",
                "entrypoint": "review",
                "files": {
                    "review": "digraph Review { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                }
            },
            "target": { "kind": "none" }
        }));
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Docker, None, None);

        let prepared = adapter
            .prepare(&client, &spec, Path::new("/host/that-must-not-be-read"))
            .await
            .expect("an extensionless inline entrypoint names a supplied file, not a selector");

        registration.assert_calls_async(1).await;
        let registered = registered.lock().unwrap();
        assert_eq!(prepared.workflow_version_id, registered[0].id().unwrap());
        assert_eq!(registered[0].entrypoint().as_str(), "review");
    }

    #[tokio::test]
    async fn workflow_version_stored_create_skips_registration_and_inherits_worker_branch() {
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
        // The child follows the parent's branch so commits the parent pushed
        // are visible; the parent's pinned commit and tag are not inherited.
        assert_eq!(
            prepared.target,
            RunTarget::Git(GitRunTarget {
                repo:   "fabro-sh/fabro".to_string(),
                branch: "main".to_string(),
                tag:    None,
                sha:    None,
            })
        );
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
    async fn workflow_version_clone_based_workers_reject_explicit_folder_targets() {
        let client = no_proxy_client("http://127.0.0.1:9");
        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "target": { "kind": "folder", "path": "/srv/server-workspace" }
        }));

        for provider in [EnvironmentProvider::Docker, EnvironmentProvider::Daytona] {
            let adapter = ServerRunCreateAdapter::worker(provider, None, None);
            let error = adapter
                .prepare(&client, &spec, Path::new("/ignored"))
                .await
                .expect_err("clone-based workers must not select server-host folders");

            assert!(
                error
                    .to_string()
                    .contains("cannot select server-host folders"),
                "unexpected error for {provider:?}: {error:#}"
            );
        }
    }

    #[tokio::test]
    async fn workflow_version_local_worker_accepts_explicit_folder_target() {
        let client = no_proxy_client("http://127.0.0.1:9");
        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let target = RunTarget::Folder {
            path: "/srv/server-workspace".to_string(),
        };
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "target": target
        }));
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Local, None, None);

        let prepared = adapter
            .prepare(&client, &spec, Path::new("/ignored"))
            .await
            .unwrap();

        assert_eq!(prepared.target, target);
    }

    #[tokio::test]
    async fn workflow_version_is_registered_before_server_admission_rejection() {
        let server = MockServer::start_async().await;
        let registered = Arc::new(Mutex::new(Vec::new()));
        let registration =
            dynamic_version_registration_mock(&server, Arc::clone(&registered)).await;
        let admission = server
            .mock_async(|when, then| {
                when.method(POST).path("/api/v1/runs");
                then.status(422)
                    .header("content-type", "text/plain")
                    .body("server-authoritative workflow rejection");
            })
            .await;
        let client = Arc::new(no_proxy_client(&server.url("")));
        let adapter = ServerRunCreateAdapter::worker(EnvironmentProvider::Docker, None, None);
        let backend =
            ClientBackend::new(Arc::clone(&client)).with_run_create_adapter(Arc::new(adapter));
        let spec = validated_spec(&json!({
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
        let error = backend
            .create_run_from_spec(&spec, Path::new("/ignored"), None)
            .await
            .expect_err("the server should reject the semantically invalid workflow");

        registration.assert_calls_async(1).await;
        admission.assert_calls_async(1).await;
        assert_eq!(registered.lock().unwrap().len(), 1);
        assert!(
            error
                .to_string()
                .contains("server-authoritative workflow rejection"),
            "unexpected error: {error:#}"
        );
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
    async fn workflow_version_standalone_rejects_unavailable_head_before_registration_or_create() {
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
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "inline",
                "entrypoint": "workflow.fabro",
                "files": {
                    "workflow.fabro": "digraph W { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                }
            }
        }));
        let server = MockServer::start_async().await;
        mock_environment(&server, "default", "docker").await;
        let registered = Arc::new(Mutex::new(Vec::new()));
        let registration =
            dynamic_version_registration_mock(&server, Arc::clone(&registered)).await;
        let create = server
            .mock_async(|when, then| {
                when.method(POST).path("/api/v1/runs");
                then.status(500);
            })
            .await;
        let client = Arc::new(no_proxy_client(&server.url("")));
        let adapter = ServerRunCreateAdapter::standalone(None);
        let backend =
            ClientBackend::new(Arc::clone(&client)).with_run_create_adapter(Arc::new(adapter));

        let error = backend
            .create_run_from_spec(&spec, &workspace, None)
            .await
            .expect_err("an unavailable local HEAD must not degrade to a branch-only target");

        registration.assert_calls_async(0).await;
        create.assert_calls_async(0).await;
        assert!(registered.lock().unwrap().is_empty());
        assert!(
            error
                .to_string()
                .contains("exact local Git commit could not be made available"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn workflow_version_standalone_preserves_dirty_warning_for_exact_target() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let origin = temp.path().join("origin.git");
        fs::create_dir(&workspace).await.unwrap();
        run_git(temp.path(), &[
            "init",
            "--bare",
            "--quiet",
            origin.to_str().unwrap(),
        ]);
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
        let push_url = format!("file://{}", origin.display());
        run_git(&workspace, &[
            "remote", "set-url", "--push", "origin", &push_url,
        ]);
        fs::write(workspace.join("dirty.txt"), "uncommitted")
            .await
            .unwrap();

        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "environment": "sandbox"
        }));
        let server = MockServer::start_async().await;
        let environment = mock_environment(&server, "sandbox", "daytona").await;
        let client = no_proxy_client(&server.url(""));
        let adapter = ServerRunCreateAdapter::standalone(None);

        let prepared = adapter.prepare(&client, &spec, &workspace).await.unwrap();

        environment.assert_calls_async(1).await;
        let RunTarget::Git(target) = prepared.target else {
            panic!("standalone attached Git checkout should derive a Git target");
        };
        assert_eq!(target.repo, "acme/widgets");
        assert_eq!(target.branch, "feature");
        assert!(target.sha.is_some());
        assert!(
            prepared
                .warnings
                .iter()
                .any(|warning| warning.contains("uncommitted changes"))
        );
    }

    #[tokio::test]
    async fn workflow_version_standalone_local_environment_targets_the_caller_folder() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("plain");
        fs::create_dir(&workspace).await.unwrap();
        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            },
            "environment": "local"
        }));
        let server = MockServer::start_async().await;
        mock_environment(&server, "local", "local").await;
        let client = no_proxy_client(&server.url(""));
        let adapter = ServerRunCreateAdapter::standalone(None);

        let prepared = adapter.prepare(&client, &spec, &workspace).await.unwrap();

        let expected = workspace.canonicalize().unwrap();
        assert_eq!(prepared.target, RunTarget::Folder {
            path: expected.to_str().unwrap().to_string(),
        });
        assert!(prepared.warnings.is_empty());
    }

    #[tokio::test]
    async fn workflow_version_standalone_clone_environment_without_git_metadata_runs_empty() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("plain");
        fs::create_dir(&workspace).await.unwrap();
        let workflow_version_id: WorkflowVersionId = fabro_types::BlobHash::new(b"stored").into();
        let spec = validated_spec(&json!({
            "workflow": {
                "kind": "stored",
                "workflow_version_id": workflow_version_id
            }
        }));
        let server = MockServer::start_async().await;
        mock_environment(&server, "default", "docker").await;
        let client = no_proxy_client(&server.url(""));
        let adapter = ServerRunCreateAdapter::standalone(None);

        let prepared = adapter.prepare(&client, &spec, &workspace).await.unwrap();

        assert_eq!(prepared.target, RunTarget::None {});
    }

    #[tokio::test]
    async fn workflow_version_standalone_honors_configured_scm_repository_over_checkout_origin() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let origin = temp.path().join("origin.git");
        fs::create_dir(&workspace).await.unwrap();
        run_git(temp.path(), &[
            "init",
            "--bare",
            "--quiet",
            origin.to_str().unwrap(),
        ]);
        run_git(&workspace, &[
            "init",
            "--quiet",
            "--initial-branch",
            "feature",
        ]);
        run_git(&workspace, &["config", "user.name", "Fabro Test"]);
        run_git(&workspace, &["config", "user.email", "fabro@example.com"]);
        let workflow_dir = workspace.join(".fabro/workflows/demo");
        fs::create_dir_all(&workflow_dir).await.unwrap();
        fs::write(workspace.join(".fabro/project.toml"), "_version = 1\n")
            .await
            .unwrap();
        fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n[run.scm]\nowner = \"acme\"\nrepository = \"widgets\"\n",
        )
        .await
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.fabro"),
            "digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
        )
        .await
        .unwrap();
        run_git(&workspace, &["add", "."]);
        run_git(&workspace, &["commit", "--quiet", "-m", "initial"]);
        // The checkout is a fork; the workflow names the upstream repository.
        run_git(&workspace, &[
            "remote",
            "add",
            "origin",
            "https://github.com/alice/widgets.git",
        ]);
        let push_url = format!("file://{}", origin.display());
        run_git(&workspace, &[
            "remote", "set-url", "--push", "origin", &push_url,
        ]);
        let server = MockServer::start_async().await;
        mock_environment(&server, "default", "docker").await;
        let registered = Arc::new(Mutex::new(Vec::new()));
        let registration =
            dynamic_version_registration_mock(&server, Arc::clone(&registered)).await;
        let client = no_proxy_client(&server.url(""));
        let spec = validated_spec(&json!({ "workflow": "demo" }));
        let adapter = ServerRunCreateAdapter::standalone(None);

        let error = adapter
            .prepare(&client, &spec, &workspace)
            .await
            .expect_err("a fork checkout must not silently become the run's repository");

        registration.assert_calls_async(0).await;
        assert!(
            error
                .to_string()
                .contains("run.scm repository that is not the local checkout's origin"),
            "unexpected error: {error:#}"
        );
    }
}
