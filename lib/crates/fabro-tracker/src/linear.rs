use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::{BlockerRef, Issue, Tracker, execute_graphql_request};

pub const LINEAR_API_ENDPOINT: &str = "https://api.linear.app/graphql";

const BLOCKS_RELATION_TYPE: &str = "blocks";

#[derive(Clone, Debug)]
pub struct LinearConfig {
    pub api_key: String,
    pub endpoint: String,
}

impl LinearConfig {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            endpoint: LINEAR_API_ENDPOINT.to_string(),
        }
    }
}

const ISSUE_FIELDS: &str = "
    id
    identifier
    title
    description
    priority
    state { name }
    branchName
    url
    assignee { id }
    labels { nodes { name } }
    inverseRelations(first: 50) {
        nodes { type issue { id identifier state { name } } }
    }
    createdAt
    updatedAt
";

fn normalize_issue(node: &Value) -> Result<Issue, String> {
    let id = node["id"].as_str().ok_or("Missing issue id")?.to_string();
    let identifier = node["identifier"]
        .as_str()
        .ok_or("Missing issue identifier")?
        .to_string();
    let title = node["title"]
        .as_str()
        .ok_or("Missing issue title")?
        .to_string();
    let description = node["description"]
        .as_str()
        .map(std::string::ToString::to_string);
    let priority = match node["priority"].as_i64() {
        Some(0) | None => None,
        Some(n) => Some(i32::try_from(n).unwrap()),
    };
    let state = node["state"]["name"]
        .as_str()
        .ok_or("Missing issue state name")?
        .to_string();
    let branch_name = node["branchName"]
        .as_str()
        .map(std::string::ToString::to_string);
    let url = node["url"].as_str().ok_or("Missing issue url")?.to_string();
    let assignee_id = node["assignee"]["id"]
        .as_str()
        .map(std::string::ToString::to_string);

    let labels = node["labels"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str())
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default();

    let blocked_by = node["inverseRelations"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|rel| {
                    rel["type"]
                        .as_str()
                        .is_some_and(|t| t.eq_ignore_ascii_case(BLOCKS_RELATION_TYPE))
                })
                .filter_map(|rel| {
                    let issue = &rel["issue"];
                    Some(BlockerRef {
                        id: issue["id"].as_str()?.to_string(),
                        identifier: issue["identifier"].as_str()?.to_string(),
                        state: issue["state"]["name"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let created_at = node["createdAt"]
        .as_str()
        .map(std::string::ToString::to_string);
    let updated_at = node["updatedAt"]
        .as_str()
        .map(std::string::ToString::to_string);

    Ok(Issue {
        id,
        project_item_id: None,
        identifier,
        title,
        description,
        priority,
        state,
        branch_name,
        url,
        assignee_id,
        labels,
        blocked_by,
        created_at,
        updated_at,
    })
}

async fn execute_graphql(
    client: &reqwest::Client,
    config: &LinearConfig,
    query: &str,
    variables: Value,
) -> Result<Value, String> {
    execute_graphql_request(
        client,
        &config.endpoint,
        &config.api_key,
        "Linear",
        query,
        variables,
    )
    .await
}

fn extract_issues(response: &Value) -> Result<Vec<Issue>, String> {
    let nodes = response["data"]["issues"]["nodes"]
        .as_array()
        .ok_or("Missing issues nodes in response")?;

    nodes.iter().map(normalize_issue).collect()
}

/// A `Tracker` implementation backed by Linear.
pub struct LinearTracker {
    config: LinearConfig,
    client: reqwest::Client,
    project_slug: String,
}

impl LinearTracker {
    pub fn new(config: LinearConfig, client: reqwest::Client, project_slug: String) -> Self {
        Self {
            config,
            client,
            project_slug,
        }
    }
}

#[async_trait]
impl Tracker for LinearTracker {
    async fn fetch_viewer_id(&self) -> Result<String, String> {
        tracing::debug!("Fetching viewer ID from Linear");
        let query = "query { viewer { id } }";
        let response =
            execute_graphql(&self.client, &self.config, query, serde_json::json!({})).await?;

        response["data"]["viewer"]["id"]
            .as_str()
            .map(std::string::ToString::to_string)
            .ok_or_else(|| "Missing viewer id in response".to_string())
    }

    async fn create_comment(&self, issue: &Issue, body: &str) -> Result<(), String> {
        tracing::debug!(issue_id = %issue.id, "Creating comment on Linear issue");
        let query = r"
            mutation($issueId: String!, $body: String!) {
                commentCreate(input: { issueId: $issueId, body: $body }) {
                    success
                }
            }
        ";

        let variables = serde_json::json!({
            "issueId": issue.id,
            "body": body,
        });

        let response = execute_graphql(&self.client, &self.config, query, variables).await?;

        let success = response["data"]["commentCreate"]["success"]
            .as_bool()
            .unwrap_or(false);
        if !success {
            return Err("Linear commentCreate returned success: false".to_string());
        }

        Ok(())
    }

    async fn update_issue_state(&self, issue: &Issue, state_name: &str) -> Result<(), String> {
        tracing::debug!(issue_id = %issue.id, state_name, "Updating Linear issue state");
        // Step 1: Resolve state name to ID via the issue's team
        let resolve_query = r"
            query($issueId: String!, $stateName: String!) {
                issue(id: $issueId) {
                    team {
                        states(filter: { name: { eq: $stateName } }) {
                            nodes { id }
                        }
                    }
                }
            }
        ";

        let resolve_vars = serde_json::json!({
            "issueId": issue.id,
            "stateName": state_name,
        });

        let resolve_resp =
            execute_graphql(&self.client, &self.config, resolve_query, resolve_vars).await?;

        let state_id = resolve_resp["data"]["issue"]["team"]["states"]["nodes"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|node| node["id"].as_str())
            .ok_or_else(|| format!("State '{state_name}' not found for issue {}", issue.id))?
            .to_string();

        // Step 2: Update the issue
        let update_query = r"
            mutation($issueId: String!, $stateId: String!) {
                issueUpdate(id: $issueId, input: { stateId: $stateId }) {
                    success
                }
            }
        ";

        let update_vars = serde_json::json!({
            "issueId": issue.id,
            "stateId": state_id,
        });

        let update_resp =
            execute_graphql(&self.client, &self.config, update_query, update_vars).await?;

        let success = update_resp["data"]["issueUpdate"]["success"]
            .as_bool()
            .unwrap_or(false);
        if !success {
            return Err("Linear issueUpdate returned success: false".to_string());
        }

        Ok(())
    }

    async fn fetch_candidate_issues(&self, state_names: &[&str]) -> Result<Vec<Issue>, String> {
        tracing::debug!(
            project_slug = %self.project_slug,
            ?state_names,
            "Fetching candidate issues from Linear"
        );

        let query = format!(
            r"
            query($slug: String!, $states: [String!]!, $cursor: String) {{
                issues(
                    first: 50
                    after: $cursor
                    filter: {{
                        project: {{ slugId: {{ eq: $slug }} }}
                        state: {{ name: {{ in: $states }} }}
                    }}
                ) {{
                    nodes {{ {ISSUE_FIELDS} }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}
            "
        );

        let mut all_issues = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let variables = serde_json::json!({
                "slug": self.project_slug,
                "states": state_names,
                "cursor": cursor,
            });

            let response = execute_graphql(&self.client, &self.config, &query, variables).await?;

            all_issues.extend(extract_issues(&response)?);

            let page_info = &response["data"]["issues"]["pageInfo"];
            if page_info["hasNextPage"].as_bool() == Some(true) {
                cursor = page_info["endCursor"]
                    .as_str()
                    .map(std::string::ToString::to_string);
            } else {
                break;
            }
        }

        Ok(all_issues)
    }

    async fn fetch_issues_by_ids(&self, ids: &[&str]) -> Result<Vec<Issue>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        tracing::debug!(count = ids.len(), "Fetching issues by ID from Linear");

        let query = format!(
            r"
            query($ids: [ID!]!) {{
                issues(filter: {{ id: {{ in: $ids }} }}) {{
                    nodes {{ {ISSUE_FIELDS} }}
                }}
            }}
            "
        );

        let mut issue_map: HashMap<String, Issue> = HashMap::with_capacity(ids.len());

        for batch in ids.chunks(50) {
            let variables = serde_json::json!({ "ids": batch });
            let response = execute_graphql(&self.client, &self.config, &query, variables).await?;

            for issue in extract_issues(&response)? {
                issue_map.insert(issue.id.clone(), issue);
            }
        }

        // Return in the same order as the input IDs
        Ok(ids.iter().filter_map(|id| issue_map.remove(*id)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_config(server_url: &str) -> LinearConfig {
        LinearConfig {
            api_key: "lin_api_test123".to_string(),
            endpoint: format!("{server_url}/graphql"),
        }
    }

    fn make_test_issue() -> Issue {
        Issue {
            id: "issue-1".to_string(),
            project_item_id: None,
            identifier: "T-1".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: "https://linear.app/t".to_string(),
            assignee_id: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    fn complete_issue_json() -> Value {
        serde_json::json!({
            "id": "issue-1",
            "identifier": "ABC-123",
            "title": "Fix the bug",
            "description": "Detailed description",
            "priority": 2,
            "state": { "name": "In Progress" },
            "branchName": "abc-123-fix-the-bug",
            "url": "https://linear.app/team/issue/ABC-123",
            "assignee": { "id": "user-1" },
            "labels": { "nodes": [{ "name": "Bug" }, { "name": "URGENT" }] },
            "inverseRelations": {
                "nodes": [
                    {
                        "type": "blocks",
                        "issue": {
                            "id": "blocker-1",
                            "identifier": "ABC-100",
                            "state": { "name": "Done" }
                        }
                    }
                ]
            },
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-02T00:00:00.000Z"
        })
    }

    // -----------------------------------------------------------------------
    // normalize_issue
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_complete_issue() {
        let node = complete_issue_json();
        let issue = normalize_issue(&node).unwrap();

        assert_eq!(issue.id, "issue-1");
        assert!(issue.project_item_id.is_none());
        assert_eq!(issue.identifier, "ABC-123");
        assert_eq!(issue.title, "Fix the bug");
        assert_eq!(issue.description.as_deref(), Some("Detailed description"));
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.state, "In Progress");
        assert_eq!(issue.branch_name.as_deref(), Some("abc-123-fix-the-bug"));
        assert_eq!(issue.url, "https://linear.app/team/issue/ABC-123");
        assert_eq!(issue.assignee_id.as_deref(), Some("user-1"));
        assert_eq!(issue.labels, vec!["bug", "urgent"]);
        assert_eq!(issue.blocked_by.len(), 1);
        assert_eq!(issue.blocked_by[0].identifier, "ABC-100");
        assert_eq!(
            issue.created_at.as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );
        assert_eq!(
            issue.updated_at.as_deref(),
            Some("2026-01-02T00:00:00.000Z")
        );
    }

    #[test]
    fn normalize_minimal_issue() {
        let node = serde_json::json!({
            "id": "issue-2",
            "identifier": "XYZ-1",
            "title": "Minimal",
            "description": null,
            "priority": null,
            "state": { "name": "Backlog" },
            "branchName": null,
            "url": "https://linear.app/team/issue/XYZ-1",
            "assignee": null,
            "labels": { "nodes": [] },
            "inverseRelations": { "nodes": [] },
            "createdAt": null,
            "updatedAt": null
        });
        let issue = normalize_issue(&node).unwrap();

        assert_eq!(issue.id, "issue-2");
        assert!(issue.project_item_id.is_none());
        assert!(issue.description.is_none());
        assert!(issue.priority.is_none());
        assert!(issue.branch_name.is_none());
        assert!(issue.assignee_id.is_none());
        assert!(issue.labels.is_empty());
        assert!(issue.blocked_by.is_empty());
        assert!(issue.created_at.is_none());
        assert!(issue.updated_at.is_none());
    }

    #[test]
    fn normalize_labels_lowercased() {
        let node = serde_json::json!({
            "id": "id",
            "identifier": "T-1",
            "title": "t",
            "state": { "name": "Todo" },
            "url": "https://linear.app/t",
            "labels": { "nodes": [{ "name": "Feature" }, { "name": "HIGH Priority" }] },
            "inverseRelations": { "nodes": [] }
        });
        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.labels, vec!["feature", "high priority"]);
    }

    #[test]
    fn normalize_blockers_extracted() {
        let node = serde_json::json!({
            "id": "id",
            "identifier": "T-1",
            "title": "t",
            "state": { "name": "Todo" },
            "url": "https://linear.app/t",
            "labels": { "nodes": [] },
            "inverseRelations": {
                "nodes": [
                    {
                        "type": "blocks",
                        "issue": { "id": "b1", "identifier": "T-10", "state": { "name": "In Progress" } }
                    },
                    {
                        "type": "related",
                        "issue": { "id": "r1", "identifier": "T-20", "state": { "name": "Done" } }
                    }
                ]
            }
        });
        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.blocked_by.len(), 1);
        assert_eq!(issue.blocked_by[0].id, "b1");
        assert_eq!(issue.blocked_by[0].identifier, "T-10");
        assert_eq!(issue.blocked_by[0].state, "In Progress");
    }

    #[test]
    fn normalize_blocker_type_case_insensitive() {
        let node = serde_json::json!({
            "id": "id",
            "identifier": "T-1",
            "title": "t",
            "state": { "name": "Todo" },
            "url": "https://linear.app/t",
            "labels": { "nodes": [] },
            "inverseRelations": {
                "nodes": [
                    {
                        "type": "Blocks",
                        "issue": { "id": "b1", "identifier": "T-5", "state": { "name": "Done" } }
                    }
                ]
            }
        });
        let issue = normalize_issue(&node).unwrap();
        assert_eq!(issue.blocked_by.len(), 1);
    }

    #[test]
    fn normalize_no_blockers() {
        let node = serde_json::json!({
            "id": "id",
            "identifier": "T-1",
            "title": "t",
            "state": { "name": "Todo" },
            "url": "https://linear.app/t",
            "labels": { "nodes": [] },
            "inverseRelations": { "nodes": [] }
        });
        let issue = normalize_issue(&node).unwrap();
        assert!(issue.blocked_by.is_empty());
    }

    #[test]
    fn normalize_priority_zero_is_none() {
        let node = serde_json::json!({
            "id": "id",
            "identifier": "T-1",
            "title": "t",
            "priority": 0,
            "state": { "name": "Todo" },
            "url": "https://linear.app/t",
            "labels": { "nodes": [] },
            "inverseRelations": { "nodes": [] }
        });
        let issue = normalize_issue(&node).unwrap();
        assert!(issue.priority.is_none());
    }

    // -----------------------------------------------------------------------
    // execute_graphql
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_graphql_success() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        let mock = server
            .mock("POST", "/graphql")
            .match_header("Authorization", "lin_api_test123")
            .match_header("Content-Type", "application/json")
            .with_status(200)
            .with_body(r#"{"data": {"viewer": {"id": "user-1"}}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = execute_graphql(
            &client,
            &config,
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        assert_eq!(result["data"]["viewer"]["id"], "user-1");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn execute_graphql_http_401() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = execute_graphql(
            &client,
            &config,
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(err.contains("401"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_graphql_http_500() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = execute_graphql(
            &client,
            &config,
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(err.contains("500"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_graphql_errors_array() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": null, "errors": [{"message": "Variable not found"}]}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = execute_graphql(&client, &config, "query { bad }", serde_json::json!({}))
            .await
            .unwrap_err();

        assert!(err.contains("Variable not found"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_graphql_correct_headers() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        let mock = server
            .mock("POST", "/graphql")
            .match_header("Authorization", "lin_api_test123")
            .match_header("Content-Type", "application/json")
            .with_status(200)
            .with_body(r#"{"data": {}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        execute_graphql(
            &client,
            &config,
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        mock.assert_async().await;
    }

    // -----------------------------------------------------------------------
    // LinearTracker::fetch_viewer_id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_viewer_id_success() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"viewer": {"id": "user-abc"}}}"#)
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let id = tracker.fetch_viewer_id().await.unwrap();
        assert_eq!(id, "user-abc");
    }

    #[tokio::test]
    async fn fetch_viewer_id_error() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let err = tracker.fetch_viewer_id().await.unwrap_err();
        assert!(err.contains("401"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // LinearTracker::create_comment
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_comment_success() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"commentCreate": {"success": true}}}"#)
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issue = make_test_issue();
        tracker.create_comment(&issue, "Hello world").await.unwrap();
    }

    #[tokio::test]
    async fn create_comment_returns_false() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"commentCreate": {"success": false}}}"#)
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issue = make_test_issue();
        let err = tracker.create_comment(&issue, "Hello").await.unwrap_err();
        assert!(err.contains("success: false"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // LinearTracker::update_issue_state
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn update_issue_state_success() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        // First call: resolve state name to ID
        let resolve_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(
                r#"{"data": {"issue": {"team": {"states": {"nodes": [{"id": "state-done"}]}}}}}"#,
            )
            .create_async()
            .await;

        // Second call: update issue
        let update_mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"issueUpdate": {"success": true}}}"#)
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issue = make_test_issue();
        tracker.update_issue_state(&issue, "Done").await.unwrap();

        resolve_mock.assert_async().await;
        update_mock.assert_async().await;
    }

    #[tokio::test]
    async fn update_issue_state_not_found() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"issue": {"team": {"states": {"nodes": []}}}}}"#)
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issue = make_test_issue();
        let err = tracker
            .update_issue_state(&issue, "Nonexistent")
            .await
            .unwrap_err();
        assert!(err.contains("Nonexistent"), "got: {err}");
        assert!(err.contains("not found"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // LinearTracker::fetch_candidate_issues
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_candidate_issues_single_page() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        let issue = complete_issue_json();
        let body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [issue],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }
            }
        });

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(body.to_string())
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "my-project".to_string());
        let issues = tracker
            .fetch_candidate_issues(&["In Progress"])
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "ABC-123");
        assert!(issues[0].project_item_id.is_none());
    }

    #[tokio::test]
    async fn fetch_candidate_issues_two_pages() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        let issue1 = serde_json::json!({
            "id": "id-1", "identifier": "T-1", "title": "First",
            "state": { "name": "Todo" }, "url": "https://linear.app/t/1",
            "labels": { "nodes": [] }, "inverseRelations": { "nodes": [] }
        });
        let issue2 = serde_json::json!({
            "id": "id-2", "identifier": "T-2", "title": "Second",
            "state": { "name": "Todo" }, "url": "https://linear.app/t/2",
            "labels": { "nodes": [] }, "inverseRelations": { "nodes": [] }
        });

        let page1 = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [issue1],
                    "pageInfo": { "hasNextPage": true, "endCursor": "cursor-1" }
                }
            }
        });
        let page2 = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [issue2],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }
            }
        });

        server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"variables":{"cursor":null}}"#.to_string(),
            ))
            .with_status(200)
            .with_body(page1.to_string())
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"variables":{"cursor":"cursor-1"}}"#.to_string(),
            ))
            .with_status(200)
            .with_body(page2.to_string())
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "T-1");
        assert_eq!(issues[1].identifier, "T-2");
    }

    #[tokio::test]
    async fn fetch_candidate_issues_empty() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        let body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }
            }
        });

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(body.to_string())
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();

        assert!(issues.is_empty());
    }

    // -----------------------------------------------------------------------
    // LinearTracker::fetch_issues_by_ids
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_issues_by_ids_ordering() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        // API returns in different order than requested
        let body = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [
                        {
                            "id": "id-b", "identifier": "T-2", "title": "B",
                            "state": { "name": "Todo" }, "url": "https://linear.app/t/2",
                            "labels": { "nodes": [] }, "inverseRelations": { "nodes": [] }
                        },
                        {
                            "id": "id-a", "identifier": "T-1", "title": "A",
                            "state": { "name": "Todo" }, "url": "https://linear.app/t/1",
                            "labels": { "nodes": [] }, "inverseRelations": { "nodes": [] }
                        }
                    ]
                }
            }
        });

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(body.to_string())
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issues = tracker
            .fetch_issues_by_ids(&["id-a", "id-b"])
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "id-a");
        assert_eq!(issues[1].id, "id-b");
    }

    #[tokio::test]
    async fn fetch_issues_by_ids_batching() {
        let mut server = mockito::Server::new_async().await;
        let config = mock_config(&server.url());

        // Create 51 IDs to trigger 2 batches
        let ids: Vec<String> = (0..51).map(|i| format!("id-{i}")).collect();
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();

        // Second batch (ids 50..51) — registered first for LIFO
        let batch2_node = serde_json::json!({
            "id": "id-50", "identifier": "T-50", "title": "T50",
            "state": { "name": "Todo" }, "url": "https://linear.app/t/50",
            "labels": { "nodes": [] }, "inverseRelations": { "nodes": [] }
        });
        let batch2 = serde_json::json!({
            "data": { "issues": { "nodes": [batch2_node] } }
        });
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(batch2.to_string())
            .create_async()
            .await;

        // First batch (ids 0..50)
        let batch1_nodes: Vec<Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "id": format!("id-{i}"),
                    "identifier": format!("T-{i}"),
                    "title": format!("T{i}"),
                    "state": { "name": "Todo" },
                    "url": format!("https://linear.app/t/{i}"),
                    "labels": { "nodes": [] },
                    "inverseRelations": { "nodes": [] }
                })
            })
            .collect();
        let batch1 = serde_json::json!({
            "data": { "issues": { "nodes": batch1_nodes } }
        });
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(batch1.to_string())
            .create_async()
            .await;

        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issues = tracker.fetch_issues_by_ids(&id_refs).await.unwrap();

        assert_eq!(issues.len(), 51);
        assert_eq!(issues[0].id, "id-0");
        assert_eq!(issues[50].id, "id-50");
    }

    #[tokio::test]
    async fn fetch_issues_by_ids_empty() {
        let config = LinearConfig::new("unused".to_string());
        let tracker = LinearTracker::new(config, reqwest::Client::new(), "proj".to_string());
        let issues = tracker.fetch_issues_by_ids(&[]).await.unwrap();
        assert!(issues.is_empty());
    }
}
