use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use git2::{
    Cred, Direction, ErrorClass, ErrorCode, FetchOptions, FileMode, Oid, PushOptions,
    RemoteCallbacks, Repository, Signature,
};
use tokio::task::{self, JoinError};

use crate::git::{GitAuthor, META_BRANCH_PREFIX};
use crate::run_dump::RunDump;
use crate::run_options::RunOptions;

pub(crate) const METADATA_PERMISSIONS: &[(&str, &str)] = &[("contents", "write")];

pub(crate) fn metadata_branch_name(run_id: &str) -> String {
    format!("{META_BRANCH_PREFIX}{run_id}")
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunMetadataError {
    #[error("metadata writer initialization failed: {0}")]
    Init(String),
    #[error("metadata token mint failed: {0}")]
    TokenMint(String),
    #[error("metadata remote discovery failed: {0}")]
    Discovery(String),
    #[error("metadata remote fetch failed: {0}")]
    Fetch(String),
    #[error("invalid metadata path: {0}")]
    InvalidPath(String),
    #[error("metadata dump serialization failed: {0}")]
    DumpSerialize(anyhow::Error),
    #[error("metadata tree build failed: {0}")]
    Tree(String),
    #[error("metadata commit failed: {0}")]
    Commit(String),
    #[error("metadata writer task failed: {0}")]
    Join(JoinError),
}

#[derive(Debug)]
pub(crate) struct MetadataSnapshot {
    pub commit_sha:  String,
    pub push_error:  Option<String>,
    pub entry_count: usize,
    pub bytes:       u64,
}

pub(crate) struct RunMetadataRuntime {
    metadata_degraded:        AtomicBool,
    metadata_warning_emitted: AtomicBool,
}

impl RunMetadataRuntime {
    pub(crate) fn new() -> Self {
        Self {
            metadata_degraded:        AtomicBool::new(false),
            metadata_warning_emitted: AtomicBool::new(false),
        }
    }

    pub(crate) fn mark_metadata_degraded(&self) -> bool {
        self.metadata_degraded.store(true, Ordering::SeqCst);
        !self.metadata_warning_emitted.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn metadata_degraded(&self) -> bool {
        self.metadata_degraded.load(Ordering::SeqCst)
    }
}

impl Default for RunMetadataRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
pub(crate) trait AuthProvider: Send + Sync {
    async fn token(&self) -> Result<Option<String>, RunMetadataError>;
}

struct GitHubAuthProvider {
    creds:       fabro_github::GitHubCredentials,
    origin_url:  String,
    permissions: HashMap<String, String>,
}

impl GitHubAuthProvider {
    fn new(
        creds: fabro_github::GitHubCredentials,
        origin_url: String,
        permissions: HashMap<String, String>,
    ) -> Self {
        Self {
            creds,
            origin_url,
            permissions,
        }
    }
}

#[async_trait]
impl AuthProvider for GitHubAuthProvider {
    async fn token(&self) -> Result<Option<String>, RunMetadataError> {
        mint_token(&self.creds, &self.origin_url, &self.permissions)
            .await
            .map(Some)
            .map_err(RunMetadataError::TokenMint)
    }
}

#[cfg(test)]
struct NoAuth;

#[cfg(test)]
#[async_trait]
impl AuthProvider for NoAuth {
    async fn token(&self) -> Result<Option<String>, RunMetadataError> {
        Ok(None)
    }
}

#[derive(Clone)]
pub(crate) struct RunMetadataWriterHandle {
    writer: Arc<Mutex<RunMetadataWriter>>,
    auth:   Arc<dyn AuthProvider>,
}

impl RunMetadataWriterHandle {
    pub(crate) fn new(writer: RunMetadataWriter, auth: Arc<dyn AuthProvider>) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
            auth,
        }
    }

    #[cfg(test)]
    fn remote_url_for_test(&self) -> String {
        self.writer
            .lock()
            .expect("metadata writer mutex poisoned")
            .remote_url
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        remote_url: String,
        branch: String,
        author: GitAuthor,
        fetch_depth: Option<i32>,
    ) -> Result<Self, RunMetadataError> {
        let writer = RunMetadataWriter::new(remote_url, branch, author, fetch_depth)?;
        Ok(Self::new(writer, Arc::new(NoAuth)))
    }

    pub(crate) async fn write_snapshot(
        &self,
        dump: &RunDump,
        message: &str,
    ) -> Result<MetadataSnapshot, RunMetadataError> {
        let token = self.auth.token().await?;
        let entries = dump
            .git_entries()
            .map_err(RunMetadataError::DumpSerialize)?;
        let entry_count = entries.len();
        let bytes = metadata_entries_bytes(&entries);
        let message = message.to_string();
        let writer = Arc::clone(&self.writer);

        task::spawn_blocking(move || {
            let mut guard = writer.lock().expect("metadata writer mutex poisoned");
            guard.write_snapshot_blocking(&entries, entry_count, bytes, &message, token.as_deref())
        })
        .await
        .map_err(RunMetadataError::Join)?
    }
}

pub(crate) fn build_metadata_writer(
    run_options: &RunOptions,
) -> Result<Option<RunMetadataWriterHandle>, RunMetadataError> {
    let Some(git) = run_options.pre_run_git.as_ref() else {
        return Ok(None);
    };
    let Some(meta_branch) = run_options
        .git
        .as_ref()
        .and_then(|git| git.meta_branch.as_ref())
    else {
        return Ok(None);
    };
    let Some(creds) = run_options.github_app.as_ref() else {
        return Ok(None);
    };

    let normalized_url = fabro_github::normalize_repo_origin_url(&git.origin_url);
    if !normalized_url.starts_with("https://") {
        return Ok(None);
    }
    if fabro_github::parse_github_owner_repo(&normalized_url).is_err() {
        return Ok(None);
    }

    let auth = Arc::new(GitHubAuthProvider::new(
        creds.clone(),
        normalized_url.clone(),
        metadata_permissions(),
    ));
    let writer = RunMetadataWriter::new(
        normalized_url,
        meta_branch.clone(),
        run_options.git_author(),
        Some(1),
    )?;
    Ok(Some(RunMetadataWriterHandle::new(writer, auth)))
}

fn metadata_permissions() -> HashMap<String, String> {
    METADATA_PERMISSIONS
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

pub(crate) async fn mint_token(
    creds: &fabro_github::GitHubCredentials,
    origin_url: &str,
    permissions: &HashMap<String, String>,
) -> Result<String, String> {
    if let fabro_github::GitHubCredentials::Token(token) = creds {
        return Ok(token.clone());
    }

    let normalized_url = fabro_github::normalize_repo_origin_url(origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&normalized_url)?;
    let fabro_github::GitHubCredentials::App(creds) = creds else {
        unreachable!("token credentials return early");
    };
    let jwt = fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = fabro_http::http_client().map_err(|err| err.to_string())?;
    let permissions = serde_json::to_value(permissions).map_err(|err| err.to_string())?;
    let install_url = creds.installation_url(&owner);
    fabro_github::create_installation_access_token_with_permissions_and_install_url(
        &client,
        &jwt,
        &owner,
        &repo,
        &fabro_github::github_api_base_url(),
        permissions,
        install_url.as_deref(),
    )
    .await
}

pub(crate) struct RunMetadataWriter {
    repo:        Repository,
    tempdir:     tempfile::TempDir,
    remote_url:  String,
    branch:      String,
    author:      GitAuthor,
    fetch_depth: Option<i32>,
    parent_oid:  Option<Oid>,
    discovered:  bool,
}

impl RunMetadataWriter {
    pub(crate) fn new(
        remote_url: String,
        branch: String,
        author: GitAuthor,
        fetch_depth: Option<i32>,
    ) -> Result<Self, RunMetadataError> {
        let tempdir = tempfile::tempdir()
            .map_err(|_| RunMetadataError::Init("failed to create writer tempdir".to_string()))?;
        let repo = Repository::init_bare(tempdir.path())
            .map_err(|err| RunMetadataError::Init(redact_metadata_error(&err, tempdir.path())))?;
        repo.odb()
            .and_then(|odb| odb.add_new_mempack_backend(999).map(|_| ()))
            .map_err(|err| RunMetadataError::Init(redact_metadata_error(&err, tempdir.path())))?;

        Ok(Self {
            repo,
            tempdir,
            remote_url,
            branch,
            author,
            fetch_depth,
            parent_oid: None,
            discovered: false,
        })
    }

    fn write_snapshot_blocking(
        &mut self,
        entries: &[(String, Vec<u8>)],
        entry_count: usize,
        bytes: u64,
        message: &str,
        token: Option<&str>,
    ) -> Result<MetadataSnapshot, RunMetadataError> {
        self.discover_parent(token)?;
        for (path, _) in entries {
            validate_metadata_path(path)?;
        }
        let tree_oid = build_tree(&self.repo, entries)
            .map_err(|err| RunMetadataError::Tree(self.redact_tree_error(err)))?;
        let tree = self
            .repo
            .find_tree(tree_oid)
            .map_err(|err| RunMetadataError::Tree(self.redact(&err)))?;
        let sig = Signature::now(&self.author.name, &self.author.email)
            .map_err(|err| RunMetadataError::Commit(self.redact(&err)))?;
        let mut full_message = message.to_string();
        self.author.append_footer(&mut full_message);
        let parents = self
            .parent_oid
            .iter()
            .map(|oid| self.repo.find_commit(*oid))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| RunMetadataError::Commit(self.redact(&err)))?;
        let parent_refs = parents.iter().collect::<Vec<_>>();
        let full_ref = self.full_ref();
        let commit_oid = self
            .repo
            .commit(
                Some(&full_ref),
                &sig,
                &sig,
                &full_message,
                &tree,
                &parent_refs,
            )
            .map_err(|err| RunMetadataError::Commit(self.redact(&err)))?;
        self.parent_oid = Some(commit_oid);

        let push_error = self.push(token).err();
        Ok(MetadataSnapshot {
            commit_sha: commit_oid.to_string(),
            push_error,
            entry_count,
            bytes,
        })
    }

    fn discover_parent(&mut self, token: Option<&str>) -> Result<(), RunMetadataError> {
        if self.discovered {
            return Ok(());
        }

        let full_ref = self.full_ref();
        if let Some(path) = file_remote_path(&self.remote_url) {
            let remote_repo = Repository::open(path)
                .map_err(|err| RunMetadataError::Discovery(self.redact(&err)))?;
            if remote_repo.find_reference(&full_ref).is_err() {
                self.discovered = true;
                return Ok(());
            }
            drop(remote_repo);
            self.fetch_parent(token, &full_ref)?;
            self.discovered = true;
            return Ok(());
        }

        let head_match = (|| -> Result<Option<Oid>, git2::Error> {
            let mut remote = self.repo.remote_anonymous(&self.remote_url)?;
            let connection =
                remote.connect_auth(Direction::Fetch, Some(make_callbacks(token)), None)?;
            let head = connection
                .list()?
                .iter()
                .find(|head| head.name() == full_ref)
                .map(git2::RemoteHead::oid);
            Ok(head)
        })()
        .map_err(|err| RunMetadataError::Discovery(self.redact(&err)))?;

        let Some(_) = head_match else {
            self.discovered = true;
            return Ok(());
        };

        self.fetch_parent(token, &full_ref)?;
        self.discovered = true;
        Ok(())
    }

    fn fetch_parent(
        &mut self,
        token: Option<&str>,
        full_ref: &str,
    ) -> Result<(), RunMetadataError> {
        (|| -> Result<(), git2::Error> {
            let mut remote = self.repo.remote_anonymous(&self.remote_url)?;
            let mut fetch_opts = FetchOptions::new();
            if let Some(depth) = self.fetch_depth {
                fetch_opts.depth(depth);
            }
            fetch_opts.remote_callbacks(make_callbacks(token));
            let refspec = format!("+{full_ref}:{full_ref}");
            remote.fetch(&[refspec.as_str()], Some(&mut fetch_opts), None)?;
            let tip = self.repo.find_reference(full_ref)?.peel_to_commit()?.id();
            self.parent_oid = Some(tip);
            Ok(())
        })()
        .map_err(|err| RunMetadataError::Fetch(self.redact(&err)))
    }

    fn push(&self, token: Option<&str>) -> Result<(), String> {
        (|| -> Result<(), git2::Error> {
            let mut remote = self.repo.remote_anonymous(&self.remote_url)?;
            let mut push_opts = PushOptions::new();
            push_opts.remote_callbacks(make_callbacks(token));
            let full_ref = self.full_ref();
            let refspec = format!("{full_ref}:{full_ref}");
            remote.push(&[refspec.as_str()], Some(&mut push_opts))
        })()
        .map_err(|err| self.redact(&err))
    }

    fn full_ref(&self) -> String {
        format!("refs/heads/{}", self.branch)
    }

    fn redact(&self, err: &git2::Error) -> String {
        redact_metadata_error(err, self.tempdir.path())
    }

    fn redact_tree_error(&self, err: BuildTreeError) -> String {
        match err {
            BuildTreeError::Git(err) => self.redact(&err),
            BuildTreeError::Conflict(message) => message,
        }
    }
}

fn make_callbacks(token: Option<&str>) -> RemoteCallbacks<'_> {
    let mut callbacks = RemoteCallbacks::new();
    if let Some(token) = token {
        callbacks.credentials(move |_, _, _| Cred::userpass_plaintext("x-access-token", token));
    }
    callbacks
}

fn file_remote_path(remote_url: &str) -> Option<&Path> {
    remote_url.strip_prefix("file://").map(Path::new)
}

fn metadata_entries_bytes(entries: &[(String, Vec<u8>)]) -> u64 {
    entries.iter().fold(0, |total, (_, bytes)| {
        total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    })
}

fn validate_metadata_path(path: &str) -> Result<(), RunMetadataError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if invalid {
        return Err(RunMetadataError::InvalidPath(path.to_string()));
    }
    Ok(())
}

enum BuildTreeError {
    Git(git2::Error),
    Conflict(String),
}

impl From<git2::Error> for BuildTreeError {
    fn from(value: git2::Error) -> Self {
        Self::Git(value)
    }
}

enum TreeNode {
    File(Vec<u8>),
    Dir(BTreeMap<String, Self>),
}

fn build_tree(repo: &Repository, entries: &[(String, Vec<u8>)]) -> Result<Oid, BuildTreeError> {
    let mut root = BTreeMap::new();
    for (path, bytes) in entries {
        insert_tree_node(&mut root, path, bytes.clone())?;
    }
    write_tree_node(repo, &root)
}

fn insert_tree_node(
    root: &mut BTreeMap<String, TreeNode>,
    path: &str,
    bytes: Vec<u8>,
) -> Result<(), BuildTreeError> {
    let segments = path.split('/').collect::<Vec<_>>();
    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        let node = current
            .entry((*segment).to_string())
            .or_insert_with(|| TreeNode::Dir(BTreeMap::new()));
        match node {
            TreeNode::Dir(children) => current = children,
            TreeNode::File(_) => {
                return Err(BuildTreeError::Conflict(format!(
                    "metadata path conflicts with file: {path}"
                )));
            }
        }
    }
    let filename = segments
        .last()
        .expect("validated path should have a filename");
    if matches!(current.get(*filename), Some(TreeNode::Dir(_))) {
        return Err(BuildTreeError::Conflict(format!(
            "metadata path conflicts with directory: {path}"
        )));
    }
    current.insert((*filename).to_string(), TreeNode::File(bytes));
    Ok(())
}

fn write_tree_node(
    repo: &Repository,
    children: &BTreeMap<String, TreeNode>,
) -> Result<Oid, BuildTreeError> {
    let mut builder = repo.treebuilder(None)?;
    for (name, node) in children {
        match node {
            TreeNode::File(bytes) => {
                let oid = repo.blob(bytes)?;
                builder.insert(name, oid, i32::from(FileMode::Blob))?;
            }
            TreeNode::Dir(children) => {
                let oid = write_tree_node(repo, children)?;
                builder.insert(name, oid, i32::from(FileMode::Tree))?;
            }
        }
    }
    Ok(builder.write()?)
}

pub(crate) fn redact_metadata_error(err: &git2::Error, tempdir: &Path) -> String {
    const MAX_ERROR_LEN: usize = 500;

    let mut message = err.message().to_string();
    message = redact_url_userinfo(&message);
    if let Some(tempdir) = tempdir.to_str() {
        message = message.replace(tempdir, "<tempdir>");
        for part in tempdir.split('/').filter(|part| !part.is_empty()) {
            if part.starts_with(".tmp") || part.starts_with("tmp") {
                message = message.replace(part, "<tempdir>");
            }
        }
    }
    let prefix = match (err.code(), err.class()) {
        (ErrorCode::Auth, _) => "github authentication failed: ",
        (ErrorCode::NotFastForward, _) => "non-fast-forward push rejected: ",
        (_, ErrorClass::Net) => "network failure: ",
        _ => "git operation failed: ",
    };
    let mut redacted = format!("{prefix}{message}");
    if redacted.len() > MAX_ERROR_LEN {
        redacted.truncate(MAX_ERROR_LEN);
        redacted.push_str("...");
    }
    redacted
}

fn redact_url_userinfo(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(scheme_pos) = rest.find("://") {
        let prefix_end = scheme_pos + 3;
        output.push_str(&rest[..prefix_end]);
        let after_scheme = &rest[prefix_end..];
        let host_end = after_scheme
            .find(|ch: char| ch == '/' || ch.is_whitespace())
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..host_end];
        if let Some((_, host)) = authority.rsplit_once('@') {
            output.push_str("***@");
            output.push_str(host);
        } else {
            output.push_str(authority);
        }
        rest = &after_scheme[host_end..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    use fabro_store::RunProjection;
    use fabro_types::{DirtyStatus, GitContext, PreRunPushOutcome, RunSpec, WorkflowSettings};
    use git2::{ErrorClass, ErrorCode};

    use super::*;
    use crate::git::GitAuthor;
    use crate::run_dump::RunDump;
    use crate::run_options::{GitCheckpointOptions, RunOptions};

    fn run_git(repo: &Path, args: &[&str]) -> String {
        String::from_utf8(run_git_bytes(repo, args))
            .unwrap()
            .trim()
            .to_string()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "metadata writer tests use synchronous git commands to inspect temporary repositories"
    )]
    fn run_git_bytes(repo: &Path, args: &[&str]) -> Vec<u8> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "metadata writer tests use synchronous git commands to set up local fake remotes"
    )]
    fn init_bare_remote() -> tempfile::TempDir {
        let remote = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "--bare"])
            .current_dir(remote.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git init --bare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        remote
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "metadata writer tests use synchronous git commands to set up temporary repositories"
    )]
    fn init_git_repo(repo: &Path) {
        let init = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(init.status.success());
        for (key, value) in [("user.name", "Test"), ("user.email", "test@test.com")] {
            let config = std::process::Command::new("git")
                .args(["config", key, value])
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(config.status.success());
        }
        let commit = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(commit.status.success());
    }

    fn file_url(path: &Path) -> String {
        format!("file://{}", path.display())
    }

    fn metadata_dump() -> RunDump {
        let mut projection = RunProjection::default();
        projection.spec = Some(RunSpec {
            run_id:           fabro_types::fixtures::RUN_1,
            settings:         WorkflowSettings::default(),
            graph:            fabro_types::Graph::new("metadata"),
            workflow_slug:    Some("metadata".to_string()),
            source_directory: Some("/Users/client/project".to_string()),
            git:              Some(GitContext {
                origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        DirtyStatus::Clean,
                push_outcome: PreRunPushOutcome::NotAttempted,
            }),
            labels:           HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        });

        let mut dump = RunDump::from_projection(&projection);
        dump.add_file_bytes("binary/payload.bin", vec![0, 159, 146, 150]);
        dump.add_file_bytes("path with spaces.txt", b"quoted path\n".to_vec());
        dump
    }

    fn run_options_for_origin(origin_url: &str) -> RunOptions {
        RunOptions {
            settings:         WorkflowSettings::default(),
            run_dir:          tempfile::tempdir().unwrap().path().to_path_buf(),
            cancel_token:     None,
            run_id:           fabro_types::fixtures::RUN_1,
            labels:           HashMap::new(),
            workflow_slug:    Some("metadata".to_string()),
            github_app:       Some(fabro_github::GitHubCredentials::Token(
                "ghs_token".to_string(),
            )),
            pre_run_git:      Some(GitContext {
                origin_url:   origin_url.to_string(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        DirtyStatus::Clean,
                push_outcome: PreRunPushOutcome::NotAttempted,
            }),
            fork_source_ref:  None,
            base_branch:      None,
            display_base_sha: None,
            git:              Some(GitCheckpointOptions {
                base_sha:    None,
                run_branch:  None,
                meta_branch: Some("fabro/meta/test-run".to_string()),
            }),
        }
    }

    #[tokio::test]
    async fn metadata_writer_writes_binary_snapshot_to_fake_remote() {
        let remote = init_bare_remote();
        let branch = "fabro/meta/test-run";
        let dump = metadata_dump();
        let expected_entries = dump.git_entries().unwrap();
        let expected_entry_count = expected_entries.len();
        let expected_bytes = expected_entries
            .iter()
            .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .sum::<u64>();
        let writer = RunMetadataWriter::new(
            file_url(remote.path()),
            branch.to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();
        let handle = RunMetadataWriterHandle::new(writer, Arc::new(NoAuth));

        let snapshot = handle.write_snapshot(&dump, "checkpoint").await.unwrap();

        assert_eq!(snapshot.push_error, None);
        assert_eq!(snapshot.entry_count, expected_entry_count);
        assert_eq!(snapshot.bytes, expected_bytes);
        let commit_sha = run_git(remote.path(), &["rev-parse", branch]);
        assert_eq!(commit_sha, snapshot.commit_sha);
        assert_eq!(
            run_git_bytes(remote.path(), &[
                "show",
                &format!("{commit_sha}:binary/payload.bin")
            ]),
            vec![0, 159, 146, 150]
        );
        assert_eq!(
            run_git(remote.path(), &[
                "show",
                &format!("{commit_sha}:path with spaces.txt")
            ]),
            "quoted path"
        );
    }

    #[tokio::test]
    async fn metadata_writer_preserves_linear_history_across_snapshots() {
        let remote = init_bare_remote();
        let branch = "fabro/meta/test-run";
        let dump = metadata_dump();
        let handle = RunMetadataWriterHandle::new_for_test(
            file_url(remote.path()),
            branch.to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();

        let first = handle.write_snapshot(&dump, "checkpoint 1").await.unwrap();
        let mut second_dump = dump.clone();
        second_dump.add_file_bytes("second.txt", b"second\n".to_vec());
        let second = handle
            .write_snapshot(&second_dump, "checkpoint 2")
            .await
            .unwrap();
        let mut third_dump = second_dump.clone();
        third_dump.add_file_bytes("third.txt", b"third\n".to_vec());
        let third = handle
            .write_snapshot(&third_dump, "checkpoint 3")
            .await
            .unwrap();

        let history = run_git(remote.path(), &["rev-list", "--parents", branch]);
        let lines = history.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            format!("{} {}", third.commit_sha, second.commit_sha)
        );
        assert_eq!(
            lines[1],
            format!("{} {}", second.commit_sha, first.commit_sha)
        );
        assert_eq!(lines[2], first.commit_sha);
    }

    #[tokio::test]
    async fn metadata_writer_resumes_from_existing_remote_tip() {
        let remote = init_bare_remote();
        let branch = "fabro/meta/test-run";
        let first_handle = RunMetadataWriterHandle::new_for_test(
            file_url(remote.path()),
            branch.to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();
        let first = first_handle
            .write_snapshot(&metadata_dump(), "checkpoint 1")
            .await
            .unwrap();
        let mut second_dump = metadata_dump();
        second_dump.add_file_bytes("second.txt", b"second\n".to_vec());
        let second = first_handle
            .write_snapshot(&second_dump, "checkpoint 2")
            .await
            .unwrap();
        drop(first_handle);

        let resumed_handle = RunMetadataWriterHandle::new_for_test(
            file_url(remote.path()),
            branch.to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();
        let mut third_dump = metadata_dump();
        third_dump.add_file_bytes("third.txt", b"third\n".to_vec());
        let third = resumed_handle
            .write_snapshot(&third_dump, "checkpoint 3")
            .await
            .unwrap();

        let parent_line = run_git(remote.path(), &[
            "rev-list",
            "--parents",
            "-n",
            "1",
            &third.commit_sha,
        ]);
        assert_eq!(
            parent_line,
            format!("{} {}", third.commit_sha, second.commit_sha)
        );
        let root_line = run_git(remote.path(), &[
            "rev-list",
            "--parents",
            "-n",
            "1",
            &first.commit_sha,
        ]);
        assert_eq!(root_line, first.commit_sha);
    }

    #[tokio::test]
    async fn metadata_writer_returns_push_error_after_local_commit() {
        let remote = tempfile::tempdir().unwrap();
        init_git_repo(remote.path());
        let handle = RunMetadataWriterHandle::new_for_test(
            file_url(remote.path()),
            "main".to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();

        let snapshot = handle
            .write_snapshot(&metadata_dump(), "checkpoint")
            .await
            .unwrap();

        assert!(!snapshot.commit_sha.is_empty());
        let push_error = snapshot.push_error.unwrap();
        assert!(push_error.contains("git operation failed"));
        assert!(push_error.contains("non-bare repos"), "{push_error}");
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "metadata writer test uses a synchronous git command to inspect a temporary remote"
    )]
    async fn metadata_writer_rejects_invalid_paths_before_commit() {
        let remote = init_bare_remote();
        let handle = RunMetadataWriterHandle::new_for_test(
            file_url(remote.path()),
            "fabro/meta/test-run".to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();
        let dump = RunDump::from_raw_entries(vec![("../escape.txt".to_string(), b"x".to_vec())]);

        let err = handle
            .write_snapshot(&dump, "checkpoint")
            .await
            .unwrap_err();

        assert!(matches!(err, RunMetadataError::InvalidPath(_)));
        let missing_ref = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "fabro/meta/test-run"])
            .current_dir(remote.path())
            .output()
            .unwrap();
        assert!(!missing_ref.status.success());
    }

    #[tokio::test]
    async fn metadata_writer_discovery_failure_is_pre_commit_error() {
        let missing = tempfile::tempdir().unwrap().path().join("missing.git");
        let handle = RunMetadataWriterHandle::new_for_test(
            file_url(&missing),
            "fabro/meta/test-run".to_string(),
            GitAuthor::default(),
            None,
        )
        .unwrap();

        let err = handle
            .write_snapshot(&metadata_dump(), "checkpoint")
            .await
            .unwrap_err();

        assert!(matches!(err, RunMetadataError::Discovery(_)));
    }

    #[test]
    fn metadata_error_redaction_strips_credentials_and_tempdir() {
        let tempdir = Path::new("/var/folders/fake/.tmpXYZ");
        let err = git2::Error::from_str(
            "authenticated request to https://x-access-token:ghs_aaaaaa@github.com/owner/repo.git in /var/folders/fake/.tmpXYZ/objects failed",
        );

        let redacted = redact_metadata_error(&err, tempdir);

        assert!(!redacted.contains("ghs_aaaaaa"));
        assert!(!redacted.contains("/var/folders/fake/.tmpXYZ"));
        assert!(!redacted.contains(".tmpXYZ"));
        assert!(redacted.contains("https://***@github.com/owner/repo.git"));
        assert!(redacted.starts_with("git operation failed:"));
    }

    #[test]
    fn metadata_error_redaction_maps_error_codes_to_stable_hints() {
        let tempdir = Path::new("/tmp/fabro");

        let auth = redact_metadata_error(
            &git2::Error::new(ErrorCode::Auth, ErrorClass::Net, "bad credentials"),
            tempdir,
        );
        let non_fast_forward = redact_metadata_error(
            &git2::Error::new(ErrorCode::NotFastForward, ErrorClass::Reference, "rejected"),
            tempdir,
        );
        let network = redact_metadata_error(
            &git2::Error::new(ErrorCode::GenericError, ErrorClass::Net, "offline"),
            tempdir,
        );

        assert!(auth.starts_with("github authentication failed:"));
        assert!(non_fast_forward.starts_with("non-fast-forward push rejected:"));
        assert!(network.starts_with("network failure:"));
    }

    #[test]
    fn metadata_writer_factory_normalizes_github_urls_and_skips_non_github() {
        let cases = [
            (
                "git@github.com:owner/repo.git",
                "https://github.com/owner/repo",
            ),
            (
                "ssh://git@github.com/owner/repo.git",
                "https://github.com/owner/repo",
            ),
            (
                "https://github.com/owner/repo.git",
                "https://github.com/owner/repo",
            ),
            (
                "https://ghs_aaaaaa@github.com/owner/repo.git",
                "https://github.com/owner/repo",
            ),
            (
                "https://x-access-token:ghs_aaaaaa@github.com/owner/repo.git",
                "https://github.com/owner/repo",
            ),
        ];

        for (origin, expected) in cases {
            let options = run_options_for_origin(origin);
            let handle = build_metadata_writer(&options).unwrap().unwrap();
            assert_eq!(handle.remote_url_for_test(), expected);
            assert!(!handle.remote_url_for_test().contains("ghs_aaaaaa"));
            assert!(!handle.remote_url_for_test().contains('@'));
        }

        assert!(
            build_metadata_writer(&run_options_for_origin("https://gitlab.com/owner/repo.git"))
                .unwrap()
                .is_none()
        );
    }
}
