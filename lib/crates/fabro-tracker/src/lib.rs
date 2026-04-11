use async_trait::async_trait;

pub mod github;
pub mod linear;

pub use github::GitHubTracker;
pub use linear::{LINEAR_API_ENDPOINT, LinearOptions, LinearTracker};

/// Shared GraphQL execution used by both provider modules.
///
/// Posts a query + variables to `endpoint`, attaches the given `auth_header`
/// as the `Authorization` value, and returns the parsed JSON response.
/// Provider-specific error messages use `provider` as a label.
pub(crate) async fn execute_graphql_request(
    client: &reqwest::Client,
    endpoint: &str,
    auth_header: &str,
    provider: &str,
    query: &str,
    variables: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });

    let resp = client
        .post(endpoint)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json")
        .header("User-Agent", "fabro")
        .timeout(std::time::Duration::from_secs(30))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{provider} GraphQL request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, provider, "GraphQL API error");
        return Err(format!(
            "{provider} GraphQL API returned HTTP {status}: {body_text}"
        ));
    }

    let response: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse {provider} GraphQL response: {e}"))?;

    if let Some(errors) = response["errors"].as_array() {
        if !errors.is_empty() {
            let messages: Vec<&str> = errors
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect();
            return Err(format!(
                "{provider} GraphQL errors: {}",
                messages.join("; ")
            ));
        }
    }

    Ok(response)
}

#[derive(Debug, Clone)]
pub struct BlockerRef {
    pub id:         String,
    pub identifier: String,
    pub state:      String,
}

#[derive(Debug, Clone)]
pub struct Issue {
    /// Provider-native issue node ID.
    pub id:              String,
    /// Provider-native project-item ID for status updates.
    /// None for providers where the issue ID is sufficient (e.g. Linear).
    /// For GitHub Projects, this is the ProjectV2Item node ID.
    /// Each Tracker impl is scoped to a single project, so this is unambiguous
    /// even when an issue belongs to multiple project boards.
    pub project_item_id: Option<String>,
    /// Human-readable identifier (e.g. "ABC-123" or "#42").
    pub identifier:      String,
    pub title:           String,
    pub description:     Option<String>,
    pub priority:        Option<i32>,
    pub state:           String,
    pub branch_name:     Option<String>,
    pub url:             String,
    pub assignee_id:     Option<String>,
    pub labels:          Vec<String>,
    pub blocked_by:      Vec<BlockerRef>,
    pub created_at:      Option<String>,
    pub updated_at:      Option<String>,
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
