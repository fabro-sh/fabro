use async_trait::async_trait;

pub mod github;
pub mod linear;

pub use github::GitHubTracker;
pub use linear::{LinearConfig, LinearTracker, LINEAR_API_ENDPOINT};

#[derive(Debug, Clone)]
pub struct BlockerRef {
    pub id: String,
    pub identifier: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct Issue {
    /// Provider-native issue node ID.
    pub id: String,
    /// Provider-native project-item ID for status updates.
    /// None for providers where the issue ID is sufficient (e.g. Linear).
    /// For GitHub Projects, this is the ProjectV2Item node ID.
    /// Each Tracker impl is scoped to a single project, so this is unambiguous
    /// even when an issue belongs to multiple project boards.
    pub project_item_id: Option<String>,
    /// Human-readable identifier (e.g. "ABC-123" or "#42").
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i32>,
    pub state: String,
    pub branch_name: Option<String>,
    pub url: String,
    pub assignee_id: Option<String>,
    pub labels: Vec<String>,
    pub blocked_by: Vec<BlockerRef>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Unified interface for project management / issue tracking systems.
///
/// Implementations are constructed with provider-specific config and used as
/// `Arc<dyn Tracker>` or `Box<dyn Tracker>`.
#[async_trait]
pub trait Tracker: Send + Sync {
    /// Return the authenticated user's ID in the provider's system.
    async fn fetch_viewer_id(&self) -> Result<String, String>;

    /// Add a comment to an issue. Each impl extracts the appropriate ID.
    async fn create_comment(&self, issue: &Issue, body: &str) -> Result<(), String>;

    /// Transition an issue to a new state by name.
    /// Each impl extracts the appropriate ID (issue ID or project item ID).
    async fn update_issue_state(&self, issue: &Issue, state_name: &str) -> Result<(), String>;

    /// Fetch issues matching any of the given state names.
    /// Project identity is in the impl's config, not here.
    async fn fetch_candidate_issues(&self, state_names: &[&str]) -> Result<Vec<Issue>, String>;

    /// Fetch specific issues by their provider-native IDs (`Issue::id` values).
    async fn fetch_issues_by_ids(&self, ids: &[&str]) -> Result<Vec<Issue>, String>;
}
