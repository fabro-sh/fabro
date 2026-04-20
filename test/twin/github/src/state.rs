use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const TEST_RSA_PRIVATE_PEM: &str = include_str!("testdata/rsa_private.pem");
const TEST_RSA_PUBLIC_PEM: &str = include_str!("testdata/rsa_public.pem");

/// Configuration for a registered GitHub App (user-facing input).
#[derive(Debug, Clone)]
pub struct AppOptions {
    pub app_id:          String,
    pub slug:            String,
    pub owner_login:     String,
    pub public:          bool,
    pub private_key_pem: String,
    pub webhook_secret:  Option<String>,
}

/// Internal enriched app config with derived public key.
#[derive(Debug, Clone)]
pub struct RegisteredApp {
    pub config:         AppOptions,
    /// Derived from `private_key_pem` during `register_app`. Used for JWT
    /// verification.
    pub public_key_pem: String,
}

/// An installation of a GitHub App on a specific owner.
#[derive(Debug, Clone)]
pub struct Installation {
    pub id:           u64,
    pub app_id:       String,
    pub owner:        String,
    pub repositories: Vec<String>,
    pub suspended:    bool,
}

/// A repository in the fake.
#[derive(Debug, Clone)]
pub struct Repository {
    pub owner:          String,
    pub name:           String,
    pub branches:       Vec<String>,
    pub default_branch: String,
    pub private:        bool,
    pub git_dir:        Option<std::path::PathBuf>,
}

/// A pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number:        u64,
    pub node_id:       String,
    pub title:         String,
    pub body:          String,
    pub state:         String,
    pub draft:         bool,
    pub mergeable:     bool,
    pub additions:     u64,
    pub deletions:     u64,
    pub changed_files: u64,
    pub html_url:      String,
    pub user_login:    String,
    pub head_ref:      String,
    pub base_ref:      String,
    pub created_at:    String,
    pub updated_at:    String,
    pub auto_merge:    Option<AutoMerge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoMerge {
    pub enabled_at:   String,
    pub merge_method: String,
}

/// A GitHub Projects V2 project.
#[derive(Debug, Clone)]
pub struct Project {
    pub node_id:         String,
    pub number:          u64,
    pub owner:           String,
    pub owner_type:      OwnerType,
    pub status_field_id: String,
    pub status_options:  Vec<StatusOption>,
    pub items:           Vec<ProjectItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OwnerType {
    Organization,
    User,
}

#[derive(Debug, Clone)]
pub struct StatusOption {
    pub id:   String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ProjectItem {
    pub id:      String,
    pub status:  String,
    pub content: IssueContent,
}

#[derive(Debug, Clone)]
pub struct IssueContent {
    pub id:           String,
    pub number:       u64,
    pub title:        String,
    pub body:         String,
    pub url:          String,
    pub created_at:   String,
    pub updated_at:   String,
    pub assignee_ids: Vec<String>,
    pub labels:       Vec<String>,
}

/// A release.
#[derive(Debug, Clone)]
pub struct Release {
    pub tag_name: String,
}

/// An app manifest conversion record.
#[derive(Debug, Clone)]
pub struct ManifestConversion {
    pub code:           String,
    pub app_id:         i64,
    pub slug:           String,
    pub client_id:      String,
    pub client_secret:  String,
    pub webhook_secret: Option<String>,
    pub pem:            String,
}

/// A comment on an issue.
#[derive(Debug, Clone)]
pub struct Comment {
    pub issue_node_id: String,
    pub body:          String,
}

/// Stores webhook configuration.
#[derive(Debug, Clone, Default)]
pub struct WebhookOptions {
    pub url:          Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum PermissionLevel {
    #[default]
    None,
    Read,
    Write,
}

impl PermissionLevel {
    fn from_json(value: Option<&serde_json::Value>) -> Self {
        match value.and_then(|value| value.as_str()) {
            Some("read") => Self::Read,
            Some("write") => Self::Write,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPermission {
    Contents,
    PullRequests,
    Issues,
    OrganizationProjects,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenPermissions {
    pub contents:              PermissionLevel,
    pub pull_requests:         PermissionLevel,
    pub issues:                PermissionLevel,
    pub organization_projects: PermissionLevel,
}

impl TokenPermissions {
    pub fn from_json(value: &serde_json::Value) -> Self {
        Self {
            contents:              PermissionLevel::from_json(value.get("contents")),
            pull_requests:         PermissionLevel::from_json(value.get("pull_requests")),
            issues:                PermissionLevel::from_json(value.get("issues")),
            organization_projects: PermissionLevel::from_json(value.get("organization_projects")),
        }
    }

    pub fn level(&self, permission: TokenPermission) -> PermissionLevel {
        match permission {
            TokenPermission::Contents => self.contents,
            TokenPermission::PullRequests => self.pull_requests,
            TokenPermission::Issues => self.issues,
            TokenPermission::OrganizationProjects => self.organization_projects,
        }
    }
}

/// Info about an active installation access token.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub app_id:          String,
    pub installation_id: u64,
    pub repositories:    Vec<String>,
    pub permissions:     serde_json::Value,
}

impl TokenInfo {
    pub fn allows_repo(&self, repo: &str) -> bool {
        self.repositories.iter().any(|allowed| allowed == repo)
    }

    pub fn parsed_permissions(&self) -> TokenPermissions {
        TokenPermissions::from_json(&self.permissions)
    }

    pub fn allows(&self, permission: TokenPermission, required: PermissionLevel) -> bool {
        self.parsed_permissions().level(permission) >= required
    }
}

/// Central in-memory state for the fake GitHub server.
#[derive(Debug, Clone)]
pub struct AppState {
    pub apps:                 HashMap<String, RegisteredApp>,
    pub installations:        Vec<Installation>,
    pub repositories:         Vec<Repository>,
    pub pull_requests:        HashMap<(String, String), Vec<PullRequest>>,
    pub active_tokens:        HashMap<String, TokenInfo>,
    pub projects:             Vec<Project>,
    pub releases:             HashMap<(String, String), Release>,
    pub manifest_conversions: HashMap<String, ManifestConversion>,
    pub comments:             Vec<Comment>,
    pub webhook_config:       WebhookOptions,
    pub next_installation_id: u64,
    pub next_pr_number:       u64,
    pub viewer_id:            String,
}

/// Derive the RSA public key PEM from a private key PEM using the openssl CLI.
///
/// Panics if openssl is not available or the key is invalid. This is acceptable
/// because the fake server is test infrastructure and openssl is already
/// required by the test helpers that generate key pairs.
#[expect(
    clippy::disallowed_types,
    reason = "test harness: piping private key PEM into openssl via sync std::io is intentional"
)]
pub fn derive_public_key_pem(private_key_pem: &str) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if private_key_pem == TEST_RSA_PRIVATE_PEM {
        return TEST_RSA_PUBLIC_PEM.to_string();
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "This synchronous test harness helper derives an RSA public key with the host openssl CLI."
    )]
    let mut child = Command::new("openssl")
        .args(["rsa", "-pubout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("openssl must be available");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(private_key_pem.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "openssl rsa -pubout failed");
    String::from_utf8(output.stdout).unwrap()
}

impl AppState {
    pub fn new() -> Self {
        Self {
            apps:                 HashMap::new(),
            installations:        Vec::new(),
            repositories:         Vec::new(),
            pull_requests:        HashMap::new(),
            active_tokens:        HashMap::new(),
            projects:             Vec::new(),
            releases:             HashMap::new(),
            manifest_conversions: HashMap::new(),
            comments:             Vec::new(),
            webhook_config:       WebhookOptions::default(),
            next_installation_id: 1,
            next_pr_number:       1,
            viewer_id:            "U_fakeviewer".to_string(),
        }
    }

    pub fn register_app(&mut self, config: AppOptions) {
        let public_key_pem = derive_public_key_pem(&config.private_key_pem);
        let app_id = config.app_id.clone();
        self.apps.insert(app_id, RegisteredApp {
            config,
            public_key_pem,
        });
    }

    pub fn add_installation(
        &mut self,
        app_id: &str,
        owner: &str,
        repos: Vec<String>,
        suspended: bool,
    ) -> u64 {
        let id = self.next_installation_id;
        self.next_installation_id += 1;
        self.installations.push(Installation {
            id,
            app_id: app_id.to_string(),
            owner: owner.to_string(),
            repositories: repos,
            suspended,
        });
        id
    }

    pub fn add_repository(
        &mut self,
        owner: &str,
        name: &str,
        branches: Vec<String>,
        private: bool,
    ) {
        self.repositories.push(Repository {
            owner: owner.to_string(),
            name: name.to_string(),
            branches,
            default_branch: "main".to_string(),
            private,
            git_dir: None,
        });
    }

    pub fn find_installation(&self, owner: &str, repo: &str) -> Option<&Installation> {
        self.installations
            .iter()
            .find(|i| i.owner == owner && i.repositories.iter().any(|r| r == repo))
    }

    pub fn find_installation_by_id(&self, id: u64) -> Option<&Installation> {
        self.installations.iter().find(|i| i.id == id)
    }

    pub fn generate_access_token(
        &mut self,
        app_id: &str,
        installation_id: u64,
        repositories: Vec<String>,
        permissions: serde_json::Value,
    ) -> String {
        let token = format!("ghs_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
        self.active_tokens.insert(token.clone(), TokenInfo {
            app_id: app_id.to_string(),
            installation_id,
            repositories,
            permissions,
        });
        token
    }

    pub fn validate_token(&self, token: &str) -> Option<&TokenInfo> {
        self.active_tokens.get(token)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize a bare git repository at `{git_root}/{owner}/{repo}.git/`.
/// Returns the path to the bare repo directory.
pub fn init_bare_repo(
    git_root: &std::path::Path,
    owner: &str,
    repo: &str,
) -> Result<std::path::PathBuf, String> {
    let repo_dir = git_root.join(owner).join(format!("{repo}.git"));
    if repo_dir.exists() {
        return Ok(repo_dir);
    }
    std::fs::create_dir_all(&repo_dir)
        .map_err(|e| format!("failed to create git dir {}: {e}", repo_dir.display()))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "This synchronous test harness helper initializes fixture git repositories with the real git CLI."
    )]
    let output = std::process::Command::new("git")
        .args(["init", "--bare"])
        .arg(&repo_dir)
        .output()
        .map_err(|e| format!("failed to run git init: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git init --bare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Enable http.receivepack so push works via git-http-backend
    #[expect(
        clippy::disallowed_methods,
        reason = "This synchronous test harness helper configures fixture git repositories with the real git CLI."
    )]
    let output = std::process::Command::new("git")
        .args(["config", "http.receivepack", "true"])
        .current_dir(&repo_dir)
        .output()
        .map_err(|e| format!("failed to configure git repo: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git config http.receivepack failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(repo_dir)
}

/// Initialize bare git repos for all repositories in the state.
/// Sets each repository's `git_dir` field.
pub fn init_git_repos(state: &mut AppState, git_root: &std::path::Path) -> Result<(), String> {
    for repo in &mut state.repositories {
        let git_dir = init_bare_repo(git_root, &repo.owner, &repo.name)?;
        repo.git_dir = Some(git_dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_rsa_private_key, test_rsa_public_key};

    #[test]
    fn empty_state_has_no_apps() {
        let state = AppState::new();
        assert!(state.apps.is_empty());
    }

    #[test]
    fn repository_has_private_field() {
        let mut state = AppState::new();
        state.add_repository("owner", "private-repo", vec!["main".to_string()], true);
        let repo = state
            .repositories
            .iter()
            .find(|r| r.name == "private-repo")
            .unwrap();
        assert!(repo.private);
    }

    #[test]
    fn git_root_initializes_bare_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let git_root = tmp.path().to_path_buf();
        let repo_git_dir = init_bare_repo(&git_root, "acme", "widgets").unwrap();
        assert!(repo_git_dir.join("HEAD").exists());
        assert!(repo_git_dir.join("objects").exists());
    }

    #[test]
    fn can_register_app() {
        let mut state = AppState::new();
        state.register_app(AppOptions {
            app_id:          "12345".to_string(),
            slug:            "test-app".to_string(),
            owner_login:     "test-owner".to_string(),
            public:          true,
            private_key_pem: test_rsa_private_key().to_string(),
            webhook_secret:  Some("secret".to_string()),
        });
        assert_eq!(state.apps.len(), 1);
        assert_eq!(state.apps["12345"].config.slug, "test-app");
        assert_eq!(state.apps["12345"].public_key_pem, test_rsa_public_key());
    }
}
