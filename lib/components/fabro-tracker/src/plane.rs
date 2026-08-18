use anyhow::{Context as _, bail};
use async_trait::async_trait;
use fabro_http::{HttpClient, Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{Issue, Tracker};

/// Configuration options for connecting to a Plane workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneOptions {
    pub api_base:  String,
    pub workspace: String,
    pub api_key:   String,
}

impl PlaneOptions {
    pub fn new(
        api_base: impl Into<String>,
        workspace: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            api_base:  api_base.into(),
            workspace: workspace.into(),
            api_key:   api_key.into(),
        }
    }

    /// Normalized API base ensuring the `/api/v1` path is present.
    pub fn normalized_api_base(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        if base.ends_with("/api/v1") {
            base.to_string()
        } else {
            format!("{base}/api/v1")
        }
    }

    /// Web base URL with `/api/v1` and trailing slashes stripped.
    pub fn web_base(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        base.trim_end_matches("/api/v1")
            .trim_end_matches('/')
            .to_string()
    }
}

/// Plane project metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneProject {
    pub id:          String,
    pub name:        String,
    #[serde(default)]
    pub identifier:  Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub network:     Option<i32>,
    #[serde(default)]
    pub created_at:  Option<String>,
    #[serde(default)]
    pub updated_at:  Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneProjectSummary {
    pub id:         String,
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default)]
    pub name:       Option<String>,
}

/// Plane lifecycle state metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaneState {
    pub id:          String,
    pub name:        String,
    #[serde(default)]
    pub group:       Option<String>,
    #[serde(default)]
    pub color:       Option<String>,
    #[serde(default)]
    pub sequence:    Option<f64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub project_id:  Option<String>,
}

/// Plane label metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneLabel {
    pub id:          String,
    pub name:        String,
    #[serde(default)]
    pub color:       Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub project_id:  Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlaneLabelOrId {
    Object {
        #[serde(default)]
        id:    Option<String>,
        #[serde(default)]
        name:  Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    Id(String),
}

/// Raw issue representation from Plane API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaneIssue {
    pub id:                   String,
    #[serde(default)]
    pub sequence_id:          Option<i64>,
    pub name:                 String,
    #[serde(default)]
    pub description_stripped: Option<String>,
    #[serde(default)]
    pub description_html:     Option<String>,
    #[serde(default)]
    pub priority:             Option<String>,
    #[serde(default)]
    pub state:                Option<serde_json::Value>,
    #[serde(default)]
    pub state_detail:         Option<PlaneState>,
    #[serde(default)]
    pub project:              Option<String>,
    #[serde(default)]
    pub project_detail:       Option<PlaneProjectSummary>,
    #[serde(default)]
    pub labels:               Option<Vec<PlaneLabelOrId>>,
    #[serde(default)]
    pub assignees:            Option<Vec<String>>,
    #[serde(default)]
    pub created_at:           Option<String>,
    #[serde(default)]
    pub updated_at:           Option<String>,
}

/// Plane issue comment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneComment {
    pub id:               String,
    #[serde(default)]
    pub comment_html:     Option<String>,
    #[serde(default)]
    pub comment_stripped: Option<String>,
    #[serde(default)]
    pub created_at:       Option<String>,
    #[serde(default)]
    pub updated_at:       Option<String>,
}

/// HTTP client for Plane REST API.
#[derive(Clone)]
pub struct PlaneClient {
    client:  HttpClient,
    options: PlaneOptions,
}

impl PlaneClient {
    /// Build a client with Fabro's proxy-aware HTTP policy. A build failure
    /// falls back to the transport default; callers can inject an explicit
    /// client with [`PlaneClient::with_client`].
    pub fn new(options: PlaneOptions) -> Self {
        let client = fabro_http::HttpClientBuilder::new()
            .build()
            .unwrap_or_default();
        Self { client, options }
    }

    pub fn with_client(client: HttpClient, options: PlaneOptions) -> Self {
        Self { client, options }
    }
    pub fn options(&self) -> &PlaneOptions {
        &self.options
    }

    /// Construct a full API URL for a subpath within the workspace.
    pub fn api_endpoint(&self, subpath: &str) -> String {
        let base = self.options.normalized_api_base();
        let ws = &self.options.workspace;
        let clean = subpath.trim_start_matches('/');
        format!("{base}/workspaces/{ws}/{clean}")
    }

    /// Construct the web URL for an issue.
    pub fn web_issue_url(&self, project_id: &str, issue_id: &str) -> String {
        let web_base = self.options.web_base();
        let ws = &self.options.workspace;
        format!("{web_base}/{ws}/projects/{project_id}/issues/{issue_id}")
    }

    /// Send a request to Plane and validate that the response is JSON,
    /// redacting sensitive body contents on auth errors.
    pub async fn request(
        &self,
        method: Method,
        subpath: &str,
        body: Option<&serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = self.api_endpoint(subpath);
        debug!(method = %method, url = %url, "Sending Plane API request");
        let mut req = self
            .client
            .request(method.clone(), &url)
            .header("X-Api-Key", &self.options.api_key)
            .header("Content-Type", "application/json")
            .header("User-Agent", "fabro")
            .timeout(std::time::Duration::from_secs(30));

        if let Some(b) = body {
            req = req.json(b);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Plane request to {subpath} failed at transport"))?;

        let status = resp.status();

        // Check for auth failures and redact bodies.
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            bail!("Plane authentication failed (HTTP {})", status.as_u16());
        }

        // Check for content-type HTML SPA fallback.
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        if content_type.contains("text/html") {
            bail!(
                "Plane returned HTML response instead of JSON. Check api_base and workspace configuration."
            );
        }

        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let preview = if body_text.len() > 300 {
                format!("{}...", &body_text[..300])
            } else {
                body_text
            };
            bail!(
                "Plane API request failed (HTTP {}): {preview}",
                status.as_u16()
            );
        }

        let text = resp
            .text()
            .await
            .with_context(|| "Failed to read Plane response body")?;

        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(serde_json::json!({}));
        }

        if trimmed.starts_with('<') {
            bail!(
                "Plane returned HTML response instead of JSON. Check api_base and workspace configuration."
            );
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&text).with_context(|| "Failed to parse Plane JSON response")?;

        Ok(parsed)
    }

    /// Execute a cursor-paginated GET request across all pages.
    pub async fn paged_request(&self, subpath: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let mut all_results = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let sep = if subpath.contains('?') { "&" } else { "?" };
            let paged_path = match &cursor {
                Some(c) => format!("{subpath}{sep}per_page=100&cursor={c}"),
                None => format!("{subpath}{sep}per_page=100"),
            };

            let resp = self.request(Method::GET, &paged_path, None).await?;

            if let Some(results) = resp.get("results").and_then(|r| r.as_array()) {
                all_results.extend(results.iter().cloned());

                let has_more = resp
                    .get("next_page_results")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);

                if !has_more {
                    break;
                }

                let next_cursor = resp
                    .get("next_cursor")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);

                if next_cursor.is_none()
                    || next_cursor == cursor
                    || next_cursor.as_deref() == Some("")
                {
                    break;
                }
                cursor = next_cursor;
            } else if let Some(arr) = resp.as_array() {
                all_results.extend(arr.iter().cloned());
                break;
            } else {
                all_results.push(resp);
                break;
            }
        }

        Ok(all_results)
    }

    /// List all projects accessible in the configured workspace.
    pub async fn list_projects(&self) -> anyhow::Result<Vec<PlaneProject>> {
        let items = self.paged_request("projects/").await?;
        let mut projects = Vec::new();
        for item in items {
            let project: PlaneProject = serde_json::from_value(item)
                .with_context(|| "Failed to deserialize PlaneProject")?;
            projects.push(project);
        }
        Ok(projects)
    }

    /// List all workflow states for a given project.
    pub async fn list_states(&self, project_id: &str) -> anyhow::Result<Vec<PlaneState>> {
        let items = self
            .paged_request(&format!("projects/{project_id}/states/"))
            .await?;
        let mut states = Vec::new();
        for item in items {
            let state: PlaneState =
                serde_json::from_value(item).with_context(|| "Failed to deserialize PlaneState")?;
            states.push(state);
        }
        Ok(states)
    }

    /// List all labels for a given project.
    pub async fn list_labels(&self, project_id: &str) -> anyhow::Result<Vec<PlaneLabel>> {
        let items = self
            .paged_request(&format!("projects/{project_id}/labels/"))
            .await?;
        let mut labels = Vec::new();
        for item in items {
            let label: PlaneLabel =
                serde_json::from_value(item).with_context(|| "Failed to deserialize PlaneLabel")?;
            labels.push(label);
        }
        Ok(labels)
    }

    /// Fetch issues in a given state for a project, normalized to `Issue`.
    pub async fn fetch_candidate_issues(
        &self,
        project_id: &str,
        state_id: &str,
    ) -> anyhow::Result<Vec<Issue>> {
        let subpath = format!("projects/{project_id}/issues/?state={state_id}");
        let items = self.paged_request(&subpath).await?;
        let mut issues = Vec::new();

        for item in items {
            let plane_issue: PlaneIssue =
                serde_json::from_value(item).with_context(|| "Failed to deserialize PlaneIssue")?;
            let normalized = self.normalize_issue(plane_issue, project_id)?;
            issues.push(normalized);
        }

        Ok(issues)
    }

    /// Fetch a single issue by project and issue ID.
    pub async fn fetch_issue(&self, project_id: &str, issue_id: &str) -> anyhow::Result<Issue> {
        let raw = self.fetch_issue_raw(project_id, issue_id).await?;
        self.normalize_issue(raw, project_id)
    }

    /// Fetch a raw `PlaneIssue`.
    pub async fn fetch_issue_raw(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> anyhow::Result<PlaneIssue> {
        let resp = self
            .request(
                Method::GET,
                &format!("projects/{project_id}/issues/{issue_id}/"),
                None,
            )
            .await?;
        let issue: PlaneIssue =
            serde_json::from_value(resp).with_context(|| "Failed to deserialize PlaneIssue")?;
        Ok(issue)
    }

    /// Update the state of an issue.
    pub async fn update_state(
        &self,
        project_id: &str,
        issue_id: &str,
        state_id: &str,
    ) -> anyhow::Result<()> {
        let body = serde_json::json!({ "state": state_id });
        self.request(
            Method::PATCH,
            &format!("projects/{project_id}/issues/{issue_id}/"),
            Some(&body),
        )
        .await
        .with_context(|| {
            format!("Failed to update state of issue {issue_id} to {state_id} in Plane")
        })?;
        Ok(())
    }

    /// Create a comment on an issue.
    pub async fn create_comment(
        &self,
        project_id: &str,
        issue_id: &str,
        comment_html: &str,
    ) -> anyhow::Result<PlaneComment> {
        let body = serde_json::json!({ "comment_html": comment_html });
        let resp = self
            .request(
                Method::POST,
                &format!("projects/{project_id}/issues/{issue_id}/comments/"),
                Some(&body),
            )
            .await
            .with_context(|| format!("Failed to create comment on issue {issue_id} in Plane"))?;
        let comment: PlaneComment =
            serde_json::from_value(resp).with_context(|| "Failed to deserialize PlaneComment")?;
        Ok(comment)
    }

    /// Add a label to an issue, preserving existing labels.
    pub async fn add_label(
        &self,
        project_id: &str,
        issue_id: &str,
        label_id: &str,
    ) -> anyhow::Result<()> {
        let raw = self.fetch_issue_raw(project_id, issue_id).await?;
        let mut label_ids = Vec::new();
        if let Some(labels) = raw.labels {
            for l in labels {
                match l {
                    PlaneLabelOrId::Object { id: Some(id), .. } | PlaneLabelOrId::Id(id) => {
                        label_ids.push(id);
                    }
                    PlaneLabelOrId::Object { .. } => {}
                }
            }
        }
        if !label_ids.iter().any(|id| id == label_id) {
            label_ids.push(label_id.to_string());
        }
        let body = serde_json::json!({ "labels": label_ids });
        self.request(
            Method::PATCH,
            &format!("projects/{project_id}/issues/{issue_id}/"),
            Some(&body),
        )
        .await
        .with_context(|| format!("Failed to add label {label_id} to issue {issue_id} in Plane"))?;
        Ok(())
    }

    /// Normalize a Plane issue into the shared `Issue` model.
    pub fn normalize_issue(&self, issue: PlaneIssue, project_id: &str) -> anyhow::Result<Issue> {
        let project_identifier = issue
            .project_detail
            .as_ref()
            .and_then(|p| p.identifier.clone())
            .unwrap_or_default();

        let identifier = match (project_identifier.as_str(), issue.sequence_id) {
            ("", Some(seq)) => format!("#{seq}"),
            (proj, Some(seq)) => format!("{proj}-{seq}"),
            ("", None) => issue.id.clone(),
            (proj, None) => format!("{proj}-{}", issue.id),
        };

        let description = issue
            .description_stripped
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                issue
                    .description_html
                    .as_deref()
                    .map(strip_html_tags)
                    .filter(|s| !s.trim().is_empty())
            });

        let priority = issue.priority.as_deref().and_then(map_priority);

        let state_name = if let Some(detail) = &issue.state_detail {
            detail.name.clone()
        } else if let Some(state_val) = &issue.state {
            if let Some(name) = state_val.get("name").and_then(|n| n.as_str()) {
                name.to_string()
            } else if let Some(s) = state_val.as_str() {
                s.to_string()
            } else {
                state_val.to_string()
            }
        } else {
            "unknown".to_string()
        };

        let mut labels = Vec::new();
        if let Some(raw_labels) = issue.labels {
            for l in raw_labels {
                match l {
                    PlaneLabelOrId::Object {
                        name: Some(name), ..
                    } => labels.push(name),
                    PlaneLabelOrId::Object { id: Some(id), .. } | PlaneLabelOrId::Id(id) => {
                        labels.push(id);
                    }
                    PlaneLabelOrId::Object { .. } => {}
                }
            }
        }
        let url = self.web_issue_url(project_id, &issue.id);
        let assignee_id = issue.assignees.and_then(|a| a.into_iter().next());

        Ok(Issue {
            id: issue.id,
            project_item_id: None,
            identifier,
            title: issue.name,
            description,
            priority,
            state: state_name,
            branch_name: None,
            url,
            assignee_id,
            labels,
            blocked_by: Vec::new(),
            created_at: issue.created_at,
            updated_at: issue.updated_at,
        })
    }
}

/// Map Plane priority string to a normalized integer priority.
fn map_priority(p: &str) -> Option<i32> {
    match p.to_lowercase().as_str() {
        "urgent" => Some(1),
        "high" => Some(2),
        "medium" => Some(3),
        "low" => Some(4),
        "none" => Some(0),
        _ => None,
    }
}

/// Minimal helper to strip HTML tags if description_stripped is absent.
fn strip_html_tags(html: &str) -> String {
    let mut in_tag = false;
    let mut text = String::with_capacity(html.len());
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A `Tracker` implementation backed by Plane.
pub struct PlaneTracker {
    client:     PlaneClient,
    project_id: String,
}

impl PlaneTracker {
    pub fn new(options: PlaneOptions, project_id: impl Into<String>) -> Self {
        Self {
            client:     PlaneClient::new(options),
            project_id: project_id.into(),
        }
    }

    pub fn with_client(client: PlaneClient, project_id: impl Into<String>) -> Self {
        Self {
            client,
            project_id: project_id.into(),
        }
    }

    pub fn client(&self) -> &PlaneClient {
        &self.client
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}

#[async_trait]
impl Tracker for PlaneTracker {
    async fn fetch_viewer_id(&self) -> anyhow::Result<String> {
        Ok("plane-integration".to_string())
    }

    async fn create_comment(&self, issue: &Issue, body: &str) -> anyhow::Result<()> {
        self.client
            .create_comment(&self.project_id, &issue.id, body)
            .await?;
        Ok(())
    }

    async fn update_issue_state(&self, issue: &Issue, state_name: &str) -> anyhow::Result<()> {
        let states = self.client.list_states(&self.project_id).await?;
        let state = states
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(state_name) || s.id == state_name)
            .with_context(|| {
                format!(
                    "State '{}' not found in Plane project {}",
                    state_name, self.project_id
                )
            })?;
        self.client
            .update_state(&self.project_id, &issue.id, &state.id)
            .await
    }

    async fn fetch_candidate_issues(&self, state_names: &[&str]) -> anyhow::Result<Vec<Issue>> {
        let states = self.client.list_states(&self.project_id).await?;
        let mut matching_state_ids = Vec::new();

        for name in state_names {
            if let Some(st) = states
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(name) || s.id == *name)
            {
                matching_state_ids.push(st.id.clone());
            }
        }

        let mut issues = Vec::new();
        for state_id in matching_state_ids {
            let mut state_issues = self
                .client
                .fetch_candidate_issues(&self.project_id, &state_id)
                .await?;
            issues.append(&mut state_issues);
        }

        Ok(issues)
    }

    async fn fetch_issues_by_ids(&self, ids: &[&str]) -> anyhow::Result<Vec<Issue>> {
        let mut issues = Vec::new();
        for id in ids {
            match self.client.fetch_issue(&self.project_id, id).await {
                Ok(issue) => issues.push(issue),
                Err(err) => {
                    warn!(issue_id = %id, error = %err, "Failed to fetch Plane issue by ID");
                }
            }
        }
        Ok(issues)
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::{GET, PATCH, POST};
    use httpmock::MockServer;
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;

    fn test_client(server_url: &str) -> PlaneClient {
        let options = PlaneOptions::new(
            format!("{server_url}/api/v1"),
            "test-workspace",
            "plane-test-key-secret-123",
        );
        PlaneClient::with_client(fabro_http::test_http_client().unwrap(), options)
    }

    #[test]
    fn url_normalization() {
        let opt1 = PlaneOptions::new("https://plane.example.com", "ws", "key");
        assert_eq!(
            opt1.normalized_api_base(),
            "https://plane.example.com/api/v1"
        );
        assert_eq!(opt1.web_base(), "https://plane.example.com");

        let opt2 = PlaneOptions::new("https://plane.example.com/api/v1/", "ws", "key");
        assert_eq!(
            opt2.normalized_api_base(),
            "https://plane.example.com/api/v1"
        );
        assert_eq!(opt2.web_base(), "https://plane.example.com");

        let client = PlaneClient::new(opt1);
        assert_eq!(
            client.api_endpoint("projects/"),
            "https://plane.example.com/api/v1/workspaces/ws/projects/"
        );
        assert_eq!(
            client.web_issue_url("proj-1", "iss-1"),
            "https://plane.example.com/ws/projects/proj-1/issues/iss-1"
        );
    }

    #[test]
    fn normalize_issue_full() {
        let client = PlaneClient::new(PlaneOptions::new(
            "https://plane.example.com/api/v1",
            "workspace-1",
            "key",
        ));

        let plane_issue = PlaneIssue {
            id:                   "issue-uuid-1".to_string(),
            sequence_id:          Some(42),
            name:                 "Fix login button bug".to_string(),
            description_stripped: Some("The button is not clickable on mobile".to_string()),
            description_html:     Some("<p>The button is not clickable on mobile</p>".to_string()),
            priority:             Some("urgent".to_string()),
            state:                None,
            state_detail:         Some(PlaneState {
                id:          "state-1".to_string(),
                name:        "Ready".to_string(),
                group:       Some("unstarted".to_string()),
                color:       Some("#ff0000".to_string()),
                sequence:    Some(1.0),
                description: None,
                project_id:  Some("proj-1".to_string()),
            }),
            project:              Some("proj-1".to_string()),
            project_detail:       Some(PlaneProjectSummary {
                id:         "proj-1".to_string(),
                identifier: Some("TIERRA".to_string()),
                name:       Some("TierraPay".to_string()),
            }),
            labels:               Some(vec![
                PlaneLabelOrId::Object {
                    id:    Some("label-1".to_string()),
                    name:  Some("bug".to_string()),
                    color: None,
                },
                PlaneLabelOrId::Id("label-uuid-2".to_string()),
            ]),
            assignees:            Some(vec!["user-uuid-1".to_string()]),
            created_at:           Some("2026-04-01T10:00:00Z".to_string()),
            updated_at:           Some("2026-04-01T11:00:00Z".to_string()),
        };

        let issue = client.normalize_issue(plane_issue, "proj-1").unwrap();
        assert_eq!(issue.id, "issue-uuid-1");
        assert_eq!(issue.identifier, "TIERRA-42");
        assert_eq!(issue.title, "Fix login button bug");
        assert_eq!(
            issue.description.as_deref(),
            Some("The button is not clickable on mobile")
        );
        assert_eq!(issue.priority, Some(1));
        assert_eq!(issue.state, "Ready");
        assert_eq!(
            issue.url,
            "https://plane.example.com/workspace-1/projects/proj-1/issues/issue-uuid-1"
        );
        assert_eq!(issue.assignee_id.as_deref(), Some("user-uuid-1"));
        assert_eq!(issue.labels, vec!["bug", "label-uuid-2"]);
    }

    #[test]
    fn normalize_issue_html_fallback() {
        let client = PlaneClient::new(PlaneOptions::new(
            "https://plane.example.com/api/v1",
            "workspace-1",
            "key",
        ));

        let plane_issue = PlaneIssue {
            id:                   "issue-2".to_string(),
            sequence_id:          Some(10),
            name:                 "HTML description issue".to_string(),
            description_stripped: None,
            description_html:     Some(
                "<h1>Title</h1>\n<p>Body paragraph with <b>bold</b> text.</p>".to_string(),
            ),
            priority:             Some("high".to_string()),
            state:                Some(json!({ "id": "s2", "name": "In Progress" })),
            state_detail:         None,
            project:              Some("proj-1".to_string()),
            project_detail:       None,
            labels:               None,
            assignees:            None,
            created_at:           None,
            updated_at:           None,
        };

        let issue = client.normalize_issue(plane_issue, "proj-1").unwrap();
        assert_eq!(issue.identifier, "#10");
        assert_eq!(
            issue.description.as_deref(),
            Some("Title Body paragraph with bold text.")
        );
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.state, "In Progress");
    }

    #[tokio::test]
    async fn request_sets_correct_headers() {
        let server = MockServer::start_async().await;
        let client = test_client(&server.url(""));

        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/")
                .header("X-Api-Key", "plane-test-key-secret-123")
                .header("Content-Type", "application/json")
                .header("User-Agent", "fabro");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "results": [
                        { "id": "p1", "name": "Project 1", "identifier": "P1" }
                    ],
                    "next_page_results": false
                }));
        });

        let projects = client.list_projects().await.unwrap();
        mock.assert();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "p1");
        assert_eq!(projects[0].name, "Project 1");
        assert_eq!(projects[0].identifier.as_deref(), Some("P1"));
    }

    #[tokio::test]
    async fn request_redacts_auth_error_body() {
        let server = MockServer::start_async().await;
        let client = test_client(&server.url(""));

        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/");
            then.status(401)
                .header("Content-Type", "application/json")
                .body("{\"error\": \"super_secret_token_in_error\"}");
        });

        let err = client
            .request(Method::GET, "projects/", None)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Plane authentication failed (HTTP 401)"));
        assert!(!msg.contains("super_secret_token"));
    }

    #[tokio::test]
    async fn request_rejects_html_spa_fallback() {
        let server = MockServer::start_async().await;
        let client = test_client(&server.url(""));

        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/");
            then.status(200)
                .header("Content-Type", "text/html; charset=utf-8")
                .body("<!DOCTYPE html><html><body><div id=\"root\"></div></body></html>");
        });

        let err = client
            .request(Method::GET, "projects/", None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Plane returned HTML response instead of JSON"));
    }
    #[tokio::test]
    async fn paged_request_follows_cursor() {
        let server = MockServer::start_async().await;
        let client = test_client(&server.url(""));

        let page1 = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/p1/states/")
                .query_param("per_page", "100")
                .query_param_missing("cursor");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "results": [
                        { "id": "s1", "name": "Backlog", "group": "backlog" }
                    ],
                    "next_page_results": true,
                    "next_cursor": "cursor_token_abc"
                }));
        });

        let page2 = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/p1/states/")
                .query_param("per_page", "100")
                .query_param("cursor", "cursor_token_abc");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "results": [
                        { "id": "s2", "name": "Done", "group": "completed" }
                    ],
                    "next_page_results": false
                }));
        });

        let states = client.list_states("p1").await.unwrap();
        page1.assert();
        page2.assert();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].name, "Backlog");
        assert_eq!(states[1].name, "Done");
    }

    #[tokio::test]
    async fn update_state_and_comment_and_labels() {
        let server = MockServer::start_async().await;
        let client = test_client(&server.url(""));

        let patch_state = server.mock(|when, then| {
            when.method(PATCH)
                .path("/api/v1/workspaces/test-workspace/projects/p1/issues/i1/")
                .json_body(json!({ "state": "s_in_progress" }));
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "id": "i1", "state": "s_in_progress" }));
        });

        let post_comment = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/workspaces/test-workspace/projects/p1/issues/i1/comments/")
                .json_body(
                    json!({ "comment_html": "<p>Fabro run started: http://localhost:3000</p>" }),
                );
            then.status(201)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "id": "c1",
                    "comment_html": "<p>Fabro run started: http://localhost:3000</p>"
                }));
        });

        client
            .update_state("p1", "i1", "s_in_progress")
            .await
            .unwrap();
        patch_state.assert();

        let comment = client
            .create_comment(
                "p1",
                "i1",
                "<p>Fabro run started: http://localhost:3000</p>",
            )
            .await
            .unwrap();
        post_comment.assert();
        assert_eq!(comment.id, "c1");
    }

    #[tokio::test]
    async fn tracker_trait_candidate_issues_and_update() {
        let server = MockServer::start_async().await;
        let options = PlaneOptions::new(
            format!("{}/api/v1", server.url("")),
            "test-workspace",
            "test-key",
        );
        let tracker = PlaneTracker::new(options, "proj-1");

        // Mock list states
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/proj-1/states/");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "results": [
                        { "id": "state-ready-id", "name": "Ready" },
                        { "id": "state-in-prog-id", "name": "In Progress" },
                        { "id": "state-done-id", "name": "Done" }
                    ],
                    "next_page_results": false
                }));
        });

        // Mock fetch issues for state-ready-id
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/workspaces/test-workspace/projects/proj-1/issues/")
                .query_param("state", "state-ready-id");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "results": [
                        {
                            "id": "iss-1",
                            "sequence_id": 101,
                            "name": "Ready ticket 1",
                            "description_stripped": "Do something",
                            "state": "state-ready-id",
                            "project_detail": { "id": "proj-1", "identifier": "TIERRA" }
                        }
                    ],
                    "next_page_results": false
                }));
        });

        // Fetch candidate issues
        let candidates = tracker.fetch_candidate_issues(&["Ready"]).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "iss-1");
        assert_eq!(candidates[0].identifier, "TIERRA-101");

        // Mock update state
        let update_mock = server.mock(|when, then| {
            when.method(PATCH)
                .path("/api/v1/workspaces/test-workspace/projects/proj-1/issues/iss-1/")
                .json_body(json!({ "state": "state-in-prog-id" }));
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "id": "iss-1", "state": "state-in-prog-id" }));
        });

        tracker
            .update_issue_state(&candidates[0], "In Progress")
            .await
            .unwrap();
        update_mock.assert();
    }

    #[tokio::test]
    async fn request_preserves_transport_source_chain() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let client = test_client(&endpoint);
        let err = client
            .request(Method::GET, "projects/", None)
            .await
            .unwrap_err();
        let chain = err.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert!(
            chain.len() >= 2,
            "expected transport source chain, got {chain:#?}"
        );
        assert!(
            chain
                .iter()
                .any(|cause| cause.contains("error sending request")),
            "expected reqwest source in chain, got {chain:#?}"
        );
    }
}
