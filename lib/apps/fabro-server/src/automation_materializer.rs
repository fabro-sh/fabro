use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use fabro_automation::{AutomationGitWorkflowSource, AutomationId};
use fabro_manifest::WorkflowVersionCollectError;
use fabro_types::{
    GitCoordinateValidationError, GitHubRepositorySlug, GitRunTarget,
    ResolvedAutomationGitWorkflowSource, RunId, RunIntent, RunIntentArgs, RunTarget,
    WorkflowVersionId,
};
use fabro_workflow_version::{WorkflowVersionStore, WorkflowVersionStoreError};
use tokio::{fs, task};

use crate::git_checkout::{
    self, GitAuthConfig, GitCheckoutError, GitCheckoutSelector, GitRepoCache, WorktreePrepareInput,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutomationRunMaterializeInput {
    pub automation_id:   AutomationId,
    pub target:          GitRunTarget,
    pub workflow_source: Option<AutomationGitWorkflowSource>,
    pub workflow:        String,
    pub run_id:          RunId,
    pub temp_root:       PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct AutomationRunMaterialized {
    pub workflow_version_id: WorkflowVersionId,
    pub target:              GitRunTarget,
    pub workflow_source:     Option<Box<ResolvedAutomationGitWorkflowSource>>,
}

impl AutomationRunMaterialized {
    /// The admission request for an automation run: the packaged workflow
    /// version at the exact checked-out target, with no caller overrides.
    pub(crate) fn into_run_intent(self, environment_id: String) -> RunIntent {
        RunIntent {
            workflow_version_id: self.workflow_version_id,
            target:              RunTarget::Git(self.target),
            args:                RunIntentArgs::default(),
            environment_id:      Some(environment_id),
            parent_id:           None,
            title:               None,
            goal:                None,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum RunMaterializeError {
    #[error("invalid automation Git target")]
    InvalidTarget {
        #[source]
        source: GitCoordinateValidationError,
    },
    #[error("invalid automation workflow source")]
    InvalidWorkflowSource {
        #[source]
        source: GitCoordinateValidationError,
    },
    #[error("failed to resolve automation {role} credentials")]
    Credentials {
        role:   CheckoutRole,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to prepare automation {role} checkout")]
    Checkout {
        role:   CheckoutRole,
        #[source]
        source: GitCheckoutError,
    },
    #[error("failed to prepare automation temporary directory {path}")]
    TempDirectory {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("automation workflow was not found")]
    WorkflowNotFound {
        #[source]
        source: WorkflowVersionCollectError,
    },
    #[error("failed to package automation workflow versions")]
    Package {
        #[source]
        source: WorkflowVersionCollectError,
    },
    #[error("workflow-version packaging task failed")]
    PackageTask {
        #[source]
        source: task::JoinError,
    },
    #[error("failed to store automation workflow versions")]
    VersionStore {
        #[source]
        source: WorkflowVersionStoreError,
    },
    #[error("failed to load server GitHub credentials")]
    LoadCredentials {
        #[source]
        source: anyhow::Error,
    },
}

/// Which repository a checkout serves; only the error message differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub(crate) enum CheckoutRole {
    #[strum(serialize = "target")]
    Target,
    #[strum(serialize = "workflow-source")]
    WorkflowSource,
}

#[async_trait]
pub(crate) trait AutomationRunMaterializer: Send + Sync {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, RunMaterializeError>;
}

#[derive(Clone)]
pub(crate) struct ProductionAutomationRunMaterializer {
    remote_resolver: Arc<dyn AutomationGitRemoteResolver>,
    repo_cache:      Arc<GitRepoCache>,
    version_store:   WorkflowVersionStore,
}

/// Where to fetch a repository from and how to authenticate.
#[derive(Clone)]
struct GitRemote {
    clone_url: String,
    auth:      Option<GitAuthConfig>,
}

#[async_trait]
trait AutomationGitRemoteResolver: Send + Sync {
    async fn resolve(&self, repo: &GitHubRepositorySlug) -> anyhow::Result<GitRemote>;
}

struct ServerGitHubRemoteResolver {
    credentials:  Option<fabro_github::GitHubCredentials>,
    api_base_url: String,
    http_client:  Option<fabro_http::HttpClient>,
}

#[async_trait]
impl AutomationGitRemoteResolver for ServerGitHubRemoteResolver {
    async fn resolve(&self, repo: &GitHubRepositorySlug) -> anyhow::Result<GitRemote> {
        let auth = git_checkout::resolve_git_read_auth_config(
            self.credentials.as_ref(),
            repo,
            &self.api_base_url,
            self.http_client.clone(),
        )
        .await?;
        Ok(GitRemote {
            clone_url: git_checkout::github_clone_url(repo),
            auth,
        })
    }
}

impl ProductionAutomationRunMaterializer {
    pub(crate) fn new(
        github_credentials: Option<fabro_github::GitHubCredentials>,
        github_api_base_url: String,
        http_client: Option<fabro_http::HttpClient>,
        repo_cache: Arc<GitRepoCache>,
        version_store: WorkflowVersionStore,
    ) -> Self {
        Self {
            remote_resolver: Arc::new(ServerGitHubRemoteResolver {
                credentials: github_credentials,
                api_base_url: github_api_base_url,
                http_client,
            }),
            repo_cache,
            version_store,
        }
    }

    #[cfg(test)]
    fn with_remote_resolver(mut self, resolver: Arc<dyn AutomationGitRemoteResolver>) -> Self {
        self.remote_resolver = resolver;
        self
    }

    async fn resolve_remote(
        &self,
        role: CheckoutRole,
        repo: &GitHubRepositorySlug,
    ) -> Result<GitRemote, RunMaterializeError> {
        self.remote_resolver
            .resolve(repo)
            .await
            .map_err(|source| RunMaterializeError::Credentials { role, source })
    }

    async fn prepare_checkout(
        &self,
        role: CheckoutRole,
        repo: &GitHubRepositorySlug,
        remote: &GitRemote,
        selector: GitCheckoutSelector<'_>,
        worktree_dir: &Path,
    ) -> Result<String, RunMaterializeError> {
        self.repo_cache
            .prepare_worktree(
                WorktreePrepareInput {
                    repo,
                    selector,
                    auth: remote.auth.as_ref(),
                    worktree_dir,
                },
                &remote.clone_url,
            )
            .await
            .map_err(|source| RunMaterializeError::Checkout { role, source })
    }
}

#[async_trait]
impl AutomationRunMaterializer for ProductionAutomationRunMaterializer {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, RunMaterializeError> {
        let validated_target = input
            .target
            .validate()
            .map_err(|source| RunMaterializeError::InvalidTarget { source })?;
        let target_repo = validated_target.repository().clone();
        let mut exact_target = validated_target.into_target();
        let workflow_source = input
            .workflow_source
            .map(GitRunTarget::validate)
            .transpose()
            .map_err(|source| RunMaterializeError::InvalidWorkflowSource { source })?;
        // A workflow source naming the target's exact coordinate shares its
        // checkout; anything else needs a second worktree.
        let separate_source = workflow_source.as_ref().filter(|source| {
            source.repository() != &target_repo
                || GitCheckoutSelector::from(source.target())
                    != GitCheckoutSelector::from(&exact_target)
        });

        fs::create_dir_all(&input.temp_root)
            .await
            .map_err(|source| RunMaterializeError::TempDirectory {
                path: input.temp_root.clone(),
                source,
            })?;
        let temp_dir = tempfile::Builder::new()
            .prefix(&format!(
                "automation-{}-{}-",
                input.automation_id.as_str(),
                input.run_id
            ))
            .tempdir_in(&input.temp_root)
            .map_err(|source| RunMaterializeError::TempDirectory {
                path: input.temp_root.clone(),
                source,
            })?;
        let target_checkout_dir = temp_dir.path().join("target");
        let target_remote = self
            .resolve_remote(CheckoutRole::Target, &target_repo)
            .await?;
        let checked_out_sha = self
            .prepare_checkout(
                CheckoutRole::Target,
                &target_repo,
                &target_remote,
                GitCheckoutSelector::from(&exact_target),
                &target_checkout_dir,
            )
            .await?;
        let (workflow_checkout_dir, workflow_checkout_sha) = match separate_source {
            None => (target_checkout_dir, checked_out_sha.clone()),
            Some(source) => {
                let repo = source.repository();
                let remote = if repo == &target_repo {
                    target_remote
                } else {
                    self.resolve_remote(CheckoutRole::WorkflowSource, repo)
                        .await?
                };
                let source_checkout_dir = temp_dir.path().join("workflow-source");
                let source_sha = self
                    .prepare_checkout(
                        CheckoutRole::WorkflowSource,
                        repo,
                        &remote,
                        GitCheckoutSelector::from(source.target()),
                        &source_checkout_dir,
                    )
                    .await?;
                (source_checkout_dir, source_sha)
            }
        };
        exact_target.sha = Some(checked_out_sha);
        let resolved_workflow_source = workflow_source.map(|source| {
            Box::new(ResolvedAutomationGitWorkflowSource::from_requested(
                source.into_target(),
                workflow_checkout_sha,
            ))
        });

        let workflow = PathBuf::from(input.workflow);
        let closure = task::spawn_blocking(move || {
            fabro_manifest::collect_workflow_versions(&workflow, &workflow_checkout_dir)
                .map_err(package_error)
        })
        .await
        .map_err(|source| RunMaterializeError::PackageTask { source })??;

        // Versions arrive dependency-first, and the store derives the same
        // content hash the collector did, so the root ID is known up front.
        for (expected, version) in closure.versions() {
            let stored = self
                .version_store
                .put(version)
                .await
                .map_err(|source| RunMaterializeError::VersionStore { source })?;
            debug_assert_eq!(
                stored, expected,
                "store and collector disagree on version ID"
            );
        }
        Ok(AutomationRunMaterialized {
            workflow_version_id: closure.root_id(),
            target:              exact_target,
            workflow_source:     resolved_workflow_source,
        })
    }
}

fn package_error(error: WorkflowVersionCollectError) -> RunMaterializeError {
    if matches!(&error, WorkflowVersionCollectError::WorkflowNotFound { .. }) {
        RunMaterializeError::WorkflowNotFound { source: error }
    } else {
        RunMaterializeError::Package { source: error }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub struct TestAutomationRunMaterializer {
    inner:         std::sync::Arc<std::sync::Mutex<TestAutomationRunMaterializerState>>,
    version_store: Option<WorkflowVersionStore>,
}

#[cfg(any(test, feature = "test-support"))]
struct TestAutomationRunMaterializerState {
    captured_inputs: Vec<AutomationRunMaterializeInput>,
    response:        Result<Box<TestMaterializedWorkflow>, TestMaterializeFailure>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
enum TestMaterializeFailure {
    InvalidTarget(GitCoordinateValidationError),
    InvalidWorkflowSource(GitCoordinateValidationError),
}

#[cfg(any(test, feature = "test-support"))]
impl From<TestMaterializeFailure> for RunMaterializeError {
    fn from(failure: TestMaterializeFailure) -> Self {
        match failure {
            TestMaterializeFailure::InvalidTarget(source) => Self::InvalidTarget { source },
            TestMaterializeFailure::InvalidWorkflowSource(source) => {
                Self::InvalidWorkflowSource { source }
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
struct TestMaterializedWorkflow {
    version: fabro_workflow_version::ValidatedWorkflowVersion,
    target:  GitRunTarget,
    store:   bool,
}

#[cfg(any(test, feature = "test-support"))]
impl TestAutomationRunMaterializer {
    pub fn succeed(target: GitRunTarget) -> Self {
        Self::new(Ok(Box::new(TestMaterializedWorkflow {
            version: test_workflow_version(),
            target,
            store: true,
        })))
    }

    pub fn return_unstored_version(target: GitRunTarget) -> Self {
        Self::new(Ok(Box::new(TestMaterializedWorkflow {
            version: test_workflow_version(),
            target,
            store: false,
        })))
    }

    pub fn fail_invalid_target() -> Self {
        Self::new(Err(TestMaterializeFailure::InvalidTarget(
            GitCoordinateValidationError::Repository,
        )))
    }

    pub fn fail_invalid_workflow_source() -> Self {
        Self::new(Err(TestMaterializeFailure::InvalidWorkflowSource(
            GitCoordinateValidationError::Branch,
        )))
    }

    fn new(response: Result<Box<TestMaterializedWorkflow>, TestMaterializeFailure>) -> Self {
        Self {
            inner:         std::sync::Arc::new(std::sync::Mutex::new(
                TestAutomationRunMaterializerState {
                    captured_inputs: Vec::new(),
                    response,
                },
            )),
            version_store: None,
        }
    }

    pub(crate) fn captured_inputs(&self) -> Vec<AutomationRunMaterializeInput> {
        self.inner
            .lock()
            .expect("test automation materializer lock poisoned")
            .captured_inputs
            .clone()
    }

    pub fn captured_workflow_sources(&self) -> Vec<Option<AutomationGitWorkflowSource>> {
        self.inner
            .lock()
            .expect("test automation materializer lock poisoned")
            .captured_inputs
            .iter()
            .map(|input| input.workflow_source.clone())
            .collect()
    }

    pub(crate) fn into_materializer(
        mut self,
        version_store: WorkflowVersionStore,
    ) -> std::sync::Arc<dyn AutomationRunMaterializer> {
        self.version_store = Some(version_store);
        std::sync::Arc::new(self)
    }
}

#[cfg(any(test, feature = "test-support"))]
fn test_workflow_version() -> fabro_workflow_version::ValidatedWorkflowVersion {
    use std::collections::BTreeMap;

    let entrypoint = fabro_types::WorkflowPath::new("workflow.fabro")
        .expect("test workflow entrypoint should be valid");
    let version = fabro_types::WorkflowVersion::new(
        entrypoint.clone(),
        BTreeMap::from([(
            entrypoint,
            "digraph Test { graph [goal=\"Test\"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }"
                .to_string(),
        )]),
        BTreeMap::new(),
    )
    .expect("test workflow version should have a valid shape");
    fabro_workflow_version::ValidatedWorkflowVersion::new(version)
        .expect("test workflow version should validate")
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl AutomationRunMaterializer for TestAutomationRunMaterializer {
    async fn materialize(
        &self,
        input: AutomationRunMaterializeInput,
    ) -> Result<AutomationRunMaterialized, RunMaterializeError> {
        let workflow_source = input.workflow_source.as_ref().map(|source| {
            Box::new(ResolvedAutomationGitWorkflowSource::from_requested(
                source.clone(),
                "ffffffffffffffffffffffffffffffffffffffff".to_string(),
            ))
        });
        let response = {
            let mut guard = self
                .inner
                .lock()
                .expect("test automation materializer lock poisoned");
            guard.captured_inputs.push(input);
            guard.response.clone()
        };
        let materialized = *response.map_err(RunMaterializeError::from)?;
        let store = self
            .version_store
            .as_ref()
            .expect("test materializer must be attached to a version store");
        let workflow_version_id = if materialized.store {
            store
                .put(&materialized.version)
                .await
                .map_err(|source| RunMaterializeError::VersionStore { source })?
        } else {
            materialized
                .version
                .version()
                .id()
                .expect("validated test workflow version should serialize canonically")
        };
        Ok(AutomationRunMaterialized {
            workflow_version_id,
            target: materialized.target,
            workflow_source,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "Materializer unit tests write small temporary workflow fixtures synchronously."
    )]

    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::*;

    const FAKE_TOKEN: &str = "ghu_automation_materializer_secret";

    struct RecordingCredentialResolver {
        repositories: Mutex<Vec<GitHubRepositorySlug>>,
        fail_for:     Option<GitHubRepositorySlug>,
    }

    impl RecordingCredentialResolver {
        fn succeeds() -> Self {
            Self {
                repositories: Mutex::new(Vec::new()),
                fail_for:     None,
            }
        }

        fn fails_for(repo: GitHubRepositorySlug) -> Self {
            Self {
                repositories: Mutex::new(Vec::new()),
                fail_for:     Some(repo),
            }
        }

        fn repositories(&self) -> Vec<String> {
            self.repositories
                .lock()
                .expect("credential recorder lock poisoned")
                .iter()
                .map(ToString::to_string)
                .collect()
        }
    }

    /// Serves local bare fixtures as clone URLs while recording which
    /// repositories had credentials resolved.
    struct FixtureRemoteResolver {
        credentials: Arc<RecordingCredentialResolver>,
        clone_urls:  HashMap<GitHubRepositorySlug, String>,
    }

    #[async_trait]
    impl AutomationGitRemoteResolver for FixtureRemoteResolver {
        async fn resolve(&self, repo: &GitHubRepositorySlug) -> anyhow::Result<GitRemote> {
            let recorder = &self.credentials;
            recorder
                .repositories
                .lock()
                .expect("credential recorder lock poisoned")
                .push(repo.clone());
            if recorder.fail_for.as_ref() == Some(repo) {
                anyhow::bail!("test repository access denied")
            }
            let clone_url = self
                .clone_urls
                .get(repo)
                .unwrap_or_else(|| panic!("no fixture clone URL for {repo}"))
                .clone();
            Ok(GitRemote {
                clone_url,
                auth: Some(GitAuthConfig::from_parts("x-access-token", FAKE_TOKEN)),
            })
        }
    }

    struct GitFixture {
        bare:        PathBuf,
        work:        PathBuf,
        initial_sha: String,
    }

    fn run_git(args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .status()
            .expect("git command should start");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn git_output(args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .output()
            .expect("git command should start");
        assert!(output.status.success(), "git command failed: {args:?}");
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_string()
    }

    fn write_workflow(work: &Path, marker: &str) {
        let workflow_dir = work.join(".fabro/workflows/demo");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::write(work.join(".fabro/project.toml"), "_version = 1\n").unwrap();
        fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.fabro"),
            format!(
                "digraph Demo {{ graph [goal=\"{marker}\"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }}\n"
            ),
        )
        .unwrap();
    }

    fn seed_repository(root: &Path, name: &str, marker: &str) -> GitFixture {
        let bare = root.join(format!("{name}.git"));
        let work = root.join(format!("{name}-work"));
        run_git(&[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ]);
        run_git(&["init", "--initial-branch=main", work.to_str().unwrap()]);
        for (key, value) in [
            ("user.email", "test@fabro.sh"),
            ("user.name", "Fabro Test"),
            ("commit.gpgsign", "false"),
        ] {
            run_git(&["-C", work.to_str().unwrap(), "config", key, value]);
        }
        write_workflow(&work, marker);
        run_git(&["-C", work.to_str().unwrap(), "add", "."]);
        run_git(&[
            "-C",
            work.to_str().unwrap(),
            "commit",
            "-m",
            "initial workflow",
        ]);
        run_git(&[
            "-C",
            work.to_str().unwrap(),
            "tag",
            "-a",
            "annotated-v1",
            "-m",
            "annotated v1",
        ]);
        run_git(&["-C", work.to_str().unwrap(), "tag", "lightweight-v1"]);
        run_git(&[
            "-C",
            work.to_str().unwrap(),
            "push",
            "--tags",
            bare.to_str().unwrap(),
            "main",
        ]);
        let initial_sha = git_output(&["-C", work.to_str().unwrap(), "rev-parse", "HEAD"]);
        GitFixture {
            bare,
            work,
            initial_sha,
        }
    }

    fn advance_repository(fixture: &GitFixture, marker: &str) -> String {
        write_workflow(&fixture.work, marker);
        run_git(&["-C", fixture.work.to_str().unwrap(), "add", "."]);
        run_git(&[
            "-C",
            fixture.work.to_str().unwrap(),
            "commit",
            "-m",
            "advance workflow",
        ]);
        run_git(&[
            "-C",
            fixture.work.to_str().unwrap(),
            "push",
            fixture.bare.to_str().unwrap(),
            "main",
        ]);
        git_output(&["-C", fixture.work.to_str().unwrap(), "rev-parse", "HEAD"])
    }

    fn repository(value: &str) -> GitHubRepositorySlug {
        GitHubRepositorySlug::try_new(value).expect("test repository should parse")
    }

    fn target(repo: &str) -> GitRunTarget {
        GitRunTarget {
            repo:   repo.to_string(),
            branch: "main".to_string(),
            tag:    None,
            sha:    None,
        }
    }

    fn source(
        repo: &str,
        branch: &str,
        tag: Option<&str>,
        sha: Option<&str>,
    ) -> AutomationGitWorkflowSource {
        AutomationGitWorkflowSource {
            repo:   repo.to_string(),
            branch: branch.to_string(),
            tag:    tag.map(str::to_string),
            sha:    sha.map(str::to_string),
        }
    }

    fn input(
        target_repo: &str,
        workflow_source: Option<AutomationGitWorkflowSource>,
        temp_root: &Path,
    ) -> AutomationRunMaterializeInput {
        AutomationRunMaterializeInput {
            automation_id: AutomationId::new("nightly").unwrap(),
            target: target(target_repo),
            workflow_source,
            workflow: "demo".to_string(),
            run_id: RunId::new(),
            temp_root: temp_root.to_path_buf(),
        }
    }

    fn test_version_store() -> WorkflowVersionStore {
        let database = fabro_store::test_support::test_database();
        WorkflowVersionStore::new(database.blobs())
    }

    fn production_materializer(
        root: &Path,
        store: WorkflowVersionStore,
        resolver: Arc<RecordingCredentialResolver>,
        clone_urls: HashMap<GitHubRepositorySlug, String>,
    ) -> ProductionAutomationRunMaterializer {
        ProductionAutomationRunMaterializer::new(
            None,
            "https://api.github.com".to_string(),
            None,
            Arc::new(GitRepoCache::new(root.join("cache"))),
            store,
        )
        .with_remote_resolver(Arc::new(FixtureRemoteResolver {
            credentials: resolver,
            clone_urls,
        }))
    }

    #[tokio::test]
    async fn coordinate_validation_errors_have_role_specific_nonduplicated_chains() {
        let temp = TempDir::new().unwrap();
        let materializer = production_materializer(
            temp.path(),
            test_version_store(),
            Arc::new(RecordingCredentialResolver::succeeds()),
            HashMap::new(),
        );

        let mut invalid_target =
            input("fabro-sh/target", None, &temp.path().join("invalid-target"));
        invalid_target.target.branch = "refs/heads/main".to_string();
        let error = materializer.materialize(invalid_target).await.unwrap_err();
        assert_eq!(fabro_util::error::collect_chain(&error), [
            "invalid automation Git target",
            "branch must be a non-empty branch name, not a ref or commit selector",
        ]);

        let error = materializer
            .materialize(input(
                "fabro-sh/target",
                Some(source("fabro-sh/workflows", "refs/heads/main", None, None)),
                &temp.path().join("invalid-source"),
            ))
            .await
            .unwrap_err();
        assert_eq!(fabro_util::error::collect_chain(&error), [
            "invalid automation workflow source",
            "branch must be a non-empty branch name, not a ref or commit selector",
        ]);
    }

    #[tokio::test]
    async fn collected_closure_stores_dependency_first_and_idempotently() {
        let temp = TempDir::new().unwrap();
        let checkout = temp.path().join("checkout");
        let workflow_dir = checkout.join(".fabro/workflows/root");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::write(
            workflow_dir.join("workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.fabro"),
            r#"digraph Root { child [stack.child_workflow="../child/workflow.fabro"] }"#,
        )
        .unwrap();
        let child_dir = checkout.join(".fabro/workflows/child");
        fs::create_dir_all(&child_dir).unwrap();
        fs::write(child_dir.join("workflow.fabro"), "digraph Child {}").unwrap();

        let closure =
            fabro_manifest::collect_workflow_versions(Path::new("root"), &checkout).unwrap();
        let store = test_version_store();

        for _ in 0..2 {
            for (expected, version) in closure.versions() {
                assert_eq!(store.put(version).await.unwrap(), expected);
            }
        }
        let loaded = store
            .get_closure(&closure.root_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.root_id(), closure.root_id());
        assert_eq!(loaded.versions().count(), 2);
    }

    #[tokio::test]
    async fn omitted_source_packages_target_checkout_and_returns_exact_target() {
        let temp = TempDir::new().unwrap();
        let target_fixture = seed_repository(temp.path(), "target", "target workflow");
        let target_repo = repository("fabro-sh/target");
        let store = test_version_store();
        let resolver = Arc::new(RecordingCredentialResolver::succeeds());
        let materializer = production_materializer(
            temp.path(),
            store.clone(),
            Arc::clone(&resolver),
            HashMap::from([(
                target_repo.clone(),
                target_fixture.bare.to_string_lossy().into_owned(),
            )]),
        );

        let materialized = materializer
            .materialize(input("fabro-sh/target", None, &temp.path().join("runs")))
            .await
            .unwrap();

        assert_eq!(
            materialized.target.sha.as_deref(),
            Some(target_fixture.initial_sha.as_str())
        );
        assert_eq!(materialized.workflow_source, None);
        let version = store
            .get(&materialized.workflow_version_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            version
                .version()
                .files()
                .values()
                .any(|contents| contents.contains("target workflow"))
        );
        assert_eq!(resolver.repositories(), vec!["fabro-sh/target"]);
    }

    #[tokio::test]
    async fn independent_source_packages_source_and_resolves_both_repositories() {
        let temp = TempDir::new().unwrap();
        let target_fixture = seed_repository(temp.path(), "target", "target workflow");
        let source_fixture = seed_repository(temp.path(), "source", "source workflow");
        let target_repo = repository("fabro-sh/target");
        let source_repo = repository("fabro-sh/workflows");
        let store = test_version_store();
        let resolver = Arc::new(RecordingCredentialResolver::succeeds());
        let materializer = production_materializer(
            temp.path(),
            store.clone(),
            Arc::clone(&resolver),
            HashMap::from([
                (
                    target_repo,
                    target_fixture.bare.to_string_lossy().into_owned(),
                ),
                (
                    source_repo,
                    source_fixture.bare.to_string_lossy().into_owned(),
                ),
            ]),
        );

        let materialized = materializer
            .materialize(input(
                "fabro-sh/target",
                Some(source("fabro-sh/workflows", "main", None, None)),
                &temp.path().join("runs"),
            ))
            .await
            .unwrap();

        assert_eq!(
            materialized.target.sha.as_deref(),
            Some(target_fixture.initial_sha.as_str())
        );
        assert_eq!(
            materialized.workflow_source,
            Some(Box::new(ResolvedAutomationGitWorkflowSource {
                repo:         "fabro-sh/workflows".to_string(),
                branch:       "main".to_string(),
                tag:          None,
                sha:          None,
                resolved_sha: source_fixture.initial_sha.clone(),
            }))
        );
        let version = store
            .get(&materialized.workflow_version_id)
            .await
            .unwrap()
            .unwrap();
        let canonical = version.version().canonical_bytes().unwrap();
        let canonical = String::from_utf8(canonical).unwrap();
        assert!(canonical.contains("source workflow"));
        assert!(!canonical.contains("target workflow"));
        assert!(!canonical.contains(&temp.path().display().to_string()));
        assert_eq!(resolver.repositories(), vec![
            "fabro-sh/target",
            "fabro-sh/workflows"
        ]);
    }

    #[tokio::test]
    async fn identical_explicit_coordinate_reuses_checkout_and_credentials() {
        let temp = TempDir::new().unwrap();
        let fixture = seed_repository(temp.path(), "shared", "shared workflow");
        let repo = repository("fabro-sh/shared");
        let store = test_version_store();
        let resolver = Arc::new(RecordingCredentialResolver::succeeds());
        let materializer = production_materializer(
            temp.path(),
            store,
            Arc::clone(&resolver),
            HashMap::from([(repo, fixture.bare.to_string_lossy().into_owned())]),
        );

        let materialized = materializer
            .materialize(input(
                "Fabro-Sh/Shared",
                Some(source("fabro-sh/shared", "main", None, None)),
                &temp.path().join("runs"),
            ))
            .await
            .unwrap();

        assert_eq!(
            materialized.workflow_source,
            Some(Box::new(ResolvedAutomationGitWorkflowSource {
                repo:         "fabro-sh/shared".to_string(),
                branch:       "main".to_string(),
                tag:          None,
                sha:          None,
                resolved_sha: fixture.initial_sha,
            }))
        );
        assert_eq!(resolver.repositories(), vec!["Fabro-Sh/Shared"]);
    }

    #[tokio::test]
    async fn same_repository_with_a_different_selector_reuses_credentials_for_a_second_worktree() {
        let temp = TempDir::new().unwrap();
        let fixture = seed_repository(temp.path(), "shared", "shared workflow");
        let repo = repository("fabro-sh/shared");
        let resolver = Arc::new(RecordingCredentialResolver::succeeds());
        let materializer = production_materializer(
            temp.path(),
            test_version_store(),
            Arc::clone(&resolver),
            HashMap::from([(repo, fixture.bare.to_string_lossy().into_owned())]),
        );

        materializer
            .materialize(input(
                "fabro-sh/shared",
                Some(source(
                    "fabro-sh/shared",
                    "main",
                    Some("annotated-v1"),
                    None,
                )),
                &temp.path().join("runs"),
            ))
            .await
            .unwrap();

        assert_eq!(resolver.repositories(), vec!["fabro-sh/shared"]);
    }

    #[tokio::test]
    async fn source_selectors_pin_commits_while_branches_advance() {
        let temp = TempDir::new().unwrap();
        let target_fixture = seed_repository(temp.path(), "target", "target workflow");
        let source_fixture = seed_repository(temp.path(), "source", "source v1");
        let store = test_version_store();
        let resolver = Arc::new(RecordingCredentialResolver::succeeds());
        let materializer = production_materializer(
            temp.path(),
            store,
            resolver,
            HashMap::from([
                (
                    repository("fabro-sh/target"),
                    target_fixture.bare.to_string_lossy().into_owned(),
                ),
                (
                    repository("fabro-sh/source"),
                    source_fixture.bare.to_string_lossy().into_owned(),
                ),
            ]),
        );
        let runs = temp.path().join("runs");
        let materialize = |branch: &str, tag: Option<&str>, sha: Option<&str>| {
            materializer.materialize(input(
                "fabro-sh/target",
                Some(source("fabro-sh/source", branch, tag, sha)),
                &runs,
            ))
        };

        let branch_v1 = materialize("main", None, None)
            .await
            .unwrap()
            .workflow_version_id;
        for tag in ["annotated-v1", "lightweight-v1"] {
            let tagged = materialize("main", Some(tag), None).await.unwrap();
            assert_eq!(tagged.workflow_version_id, branch_v1, "{tag}");
        }
        let committed_v1 = materialize(
            "branch-that-does-not-exist",
            Some("missing-tag"),
            Some(&source_fixture.initial_sha),
        )
        .await
        .unwrap()
        .workflow_version_id;
        assert_eq!(committed_v1, branch_v1);

        advance_repository(&source_fixture, "source v2");
        let branch_v2 = materialize("main", None, None)
            .await
            .unwrap()
            .workflow_version_id;
        assert_ne!(branch_v2, branch_v1);
        let committed_after_advance = materialize(
            "branch-that-does-not-exist",
            None,
            Some(&source_fixture.initial_sha),
        )
        .await
        .unwrap()
        .workflow_version_id;
        assert_eq!(committed_after_advance, committed_v1);
    }

    #[tokio::test]
    async fn target_and_source_failures_keep_distinct_error_chains_without_tokens() {
        let temp = TempDir::new().unwrap();
        let target_fixture = seed_repository(temp.path(), "target", "target workflow");
        let source_fixture = seed_repository(temp.path(), "source", "source workflow");
        let target_repo = repository("fabro-sh/target");
        let source_repo = repository("fabro-sh/source");
        let clone_urls = HashMap::from([
            (
                target_repo.clone(),
                target_fixture.bare.to_string_lossy().into_owned(),
            ),
            (
                source_repo.clone(),
                source_fixture.bare.to_string_lossy().into_owned(),
            ),
        ]);

        let mut missing_target = input(
            "fabro-sh/target",
            None,
            &temp.path().join("target-checkout-failure"),
        );
        missing_target.target.branch = "missing".to_string();
        let error = production_materializer(
            temp.path(),
            test_version_store(),
            Arc::new(RecordingCredentialResolver::succeeds()),
            clone_urls.clone(),
        )
        .materialize(missing_target)
        .await
        .unwrap_err();
        assert!(matches!(error, RunMaterializeError::Checkout {
            role:   CheckoutRole::Target,
            source: GitCheckoutError::FetchBranch { .. },
        }));
        assert!(!format!("{error:?}").contains(FAKE_TOKEN));

        let target_resolver = Arc::new(RecordingCredentialResolver::fails_for(target_repo.clone()));
        let error = production_materializer(
            temp.path(),
            test_version_store(),
            target_resolver,
            clone_urls.clone(),
        )
        .materialize(input(
            "fabro-sh/target",
            None,
            &temp.path().join("target-failure"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, RunMaterializeError::Credentials {
            role: CheckoutRole::Target,
            ..
        }));
        assert!(!format!("{error:?}").contains(FAKE_TOKEN));

        let source_resolver = Arc::new(RecordingCredentialResolver::fails_for(source_repo));
        let error = production_materializer(
            temp.path(),
            test_version_store(),
            source_resolver,
            clone_urls,
        )
        .materialize(input(
            "fabro-sh/target",
            Some(source("fabro-sh/source", "main", None, None)),
            &temp.path().join("source-failure"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, RunMaterializeError::Credentials {
            role: CheckoutRole::WorkflowSource,
            ..
        }));
        assert!(!format!("{error:?}").contains(FAKE_TOKEN));

        let error = production_materializer(
            temp.path(),
            test_version_store(),
            Arc::new(RecordingCredentialResolver::succeeds()),
            HashMap::from([
                (
                    target_repo,
                    target_fixture.bare.to_string_lossy().into_owned(),
                ),
                (
                    repository("fabro-sh/source"),
                    source_fixture.bare.to_string_lossy().into_owned(),
                ),
            ]),
        )
        .materialize(input(
            "fabro-sh/target",
            Some(source("fabro-sh/source", "missing", None, None)),
            &temp.path().join("checkout-failure"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, RunMaterializeError::Checkout {
            role:   CheckoutRole::WorkflowSource,
            source: GitCheckoutError::FetchBranch { .. },
        }));
        assert!(!format!("{error:?}").contains(FAKE_TOKEN));
    }

    #[tokio::test]
    async fn workflow_discovery_and_version_storage_failures_remain_distinct() {
        let temp = TempDir::new().unwrap();
        let fixture = seed_repository(temp.path(), "target", "target workflow");
        let repo = repository("fabro-sh/target");
        let clone_urls = HashMap::from([(repo, fixture.bare.to_string_lossy().into_owned())]);
        let mut missing = input(
            "fabro-sh/target",
            None,
            &temp.path().join("missing-workflow"),
        );
        missing.workflow = ".fabro/workflows/absent/workflow.fabro".to_string();
        let error = production_materializer(
            temp.path(),
            test_version_store(),
            Arc::new(RecordingCredentialResolver::succeeds()),
            clone_urls.clone(),
        )
        .materialize(missing)
        .await
        .unwrap_err();
        assert!(
            matches!(error, RunMaterializeError::WorkflowNotFound { .. }),
            "unexpected missing-workflow error: {error:?}"
        );

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .unwrap();
        pool.close().await;
        let failing_store = WorkflowVersionStore::new(Arc::new(fabro_store::BlobStore::new(pool)));
        let error = production_materializer(
            temp.path(),
            failing_store,
            Arc::new(RecordingCredentialResolver::succeeds()),
            clone_urls,
        )
        .materialize(input(
            "fabro-sh/target",
            None,
            &temp.path().join("store-failure"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, RunMaterializeError::VersionStore { .. }));

        fs::write(
            fixture.work.join(".fabro/workflows/demo/workflow.fabro"),
            "this is not a graph\n",
        )
        .unwrap();
        run_git(&["-C", fixture.work.to_str().unwrap(), "add", "."]);
        run_git(&[
            "-C",
            fixture.work.to_str().unwrap(),
            "commit",
            "-m",
            "break workflow",
        ]);
        run_git(&[
            "-C",
            fixture.work.to_str().unwrap(),
            "push",
            fixture.bare.to_str().unwrap(),
            "main",
        ]);
        let error = production_materializer(
            temp.path(),
            test_version_store(),
            Arc::new(RecordingCredentialResolver::succeeds()),
            HashMap::from([(
                repository("fabro-sh/target"),
                fixture.bare.to_string_lossy().into_owned(),
            )]),
        )
        .materialize(input(
            "fabro-sh/target",
            None,
            &temp.path().join("package-failure"),
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, RunMaterializeError::Package { .. }));
    }
}
