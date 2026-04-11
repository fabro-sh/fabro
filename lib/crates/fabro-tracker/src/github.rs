use async_trait::async_trait;
use fabro_github::{
    GitHubAppCredentials, create_installation_access_token_for_projects, sign_app_jwt,
};
use tokio::sync::OnceCell;

use crate::{Issue, Tracker, execute_graphql_request};

/// Execute a GitHub GraphQL request and return the response JSON.
async fn execute_github_graphql(
    client: &reqwest::Client,
    token: &str,
    endpoint: &str,
    query: &str,
    variables: serde_json::Value,
) -> Result<serde_json::Value, String> {
    execute_graphql_request(
        client,
        endpoint,
        &format!("Bearer {token}"),
        "GitHub",
        query,
        variables,
    )
    .await
}

/// A `Tracker` implementation backed by GitHub Projects V2.
///
/// Scoped to a single project board identified by `project_number`.
pub struct GitHubTracker {
    creds: GitHubAppCredentials,
    client: reqwest::Client,
    owner: String,
    repo: String,
    project_number: u64,
    base_url: String,
    project_node_id: OnceCell<String>,
}

impl GitHubTracker {
    pub fn new(
        creds: GitHubAppCredentials,
        client: reqwest::Client,
        owner: String,
        repo: String,
        project_number: u64,
        base_url: String,
    ) -> Self {
        Self {
            creds,
            client,
            owner,
            repo,
            project_number,
            base_url,
            project_node_id: OnceCell::new(),
        }
    }

    fn graphql_url(&self) -> String {
        format!("{}/graphql", self.base_url)
    }

    async fn fresh_token(&self) -> Result<String, String> {
        let jwt = sign_app_jwt(&self.creds.app_id, &self.creds.private_key_pem)?;
        create_installation_access_token_for_projects(
            &self.client,
            &jwt,
            &self.owner,
            &self.repo,
            &self.base_url,
        )
        .await
    }

    async fn resolve_project_node_id(&self, token: &str) -> Result<&str, String> {
        self.project_node_id
            .get_or_try_init(|| async {
                tracing::debug!(
                    owner = %self.owner,
                    project_number = self.project_number,
                    "Resolving GitHub project node ID"
                );
                let graphql_url = self.graphql_url();
                let query = r"
                    query($owner: String!, $number: Int!) {
                        organization(login: $owner) {
                            projectV2(number: $number) { id }
                        }
                    }
                ";
                let variables = serde_json::json!({
                    "owner": self.owner,
                    "number": self.project_number,
                });

                let resp = execute_github_graphql(
                    &self.client,
                    token,
                    &graphql_url,
                    query,
                    variables.clone(),
                )
                .await?;

                // Try org path first, fall back to user path
                if let Some(id) = resp["data"]["organization"]["projectV2"]["id"].as_str() {
                    return Ok(id.to_string());
                }

                let user_query = r"
                    query($owner: String!, $number: Int!) {
                        user(login: $owner) {
                            projectV2(number: $number) { id }
                        }
                    }
                ";
                let user_resp = execute_github_graphql(
                    &self.client,
                    token,
                    &graphql_url,
                    user_query,
                    variables,
                )
                .await?;

                user_resp["data"]["user"]["projectV2"]["id"]
                    .as_str()
                    .map(std::string::ToString::to_string)
                    .ok_or_else(|| {
                        format!(
                            "Project #{} not found for owner '{}'",
                            self.project_number, self.owner
                        )
                    })
            })
            .await
            .map(std::string::String::as_str)
    }
}

fn normalize_github_item(item: &serde_json::Value) -> Option<Issue> {
    let project_item_id = item["id"].as_str()?.to_string();
    let content = &item["content"];

    let id = content["id"].as_str()?.to_string();
    let number = content["number"].as_u64()?;
    let identifier = format!("#{number}");
    let title = content["title"].as_str()?.to_string();
    let url = content["url"].as_str()?.to_string();
    let description = content["body"]
        .as_str()
        .map(std::string::ToString::to_string);

    let state = item["fieldValueByName"]["name"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let assignee_id = content["assignees"]["nodes"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|a| a["id"].as_str())
        .map(std::string::ToString::to_string);

    let labels = content["labels"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str())
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default();

    let created_at = content["createdAt"]
        .as_str()
        .map(std::string::ToString::to_string);
    let updated_at = content["updatedAt"]
        .as_str()
        .map(std::string::ToString::to_string);

    Some(Issue {
        id,
        project_item_id: Some(project_item_id),
        identifier,
        title,
        description,
        priority: None,
        state,
        branch_name: None,
        url,
        assignee_id,
        labels,
        blocked_by: vec![],
        created_at,
        updated_at,
    })
}

/// Fetch one page of project items. Returns (items, has_next_page, end_cursor).
async fn fetch_project_items_page(
    client: &reqwest::Client,
    token: &str,
    graphql_url: &str,
    project_node_id: &str,
    cursor: Option<&str>,
) -> Result<(Vec<serde_json::Value>, bool, Option<String>), String> {
    let query = r#"
        query($projectId: ID!, $cursor: String) {
            node(id: $projectId) {
                ... on ProjectV2 {
                    items(first: 100, after: $cursor) {
                        nodes {
                            id
                            fieldValueByName(name: "Status") {
                                ... on ProjectV2ItemFieldSingleSelectValue {
                                    name
                                }
                            }
                            content {
                                ... on Issue {
                                    id
                                    number
                                    title
                                    body
                                    url
                                    createdAt
                                    updatedAt
                                    assignees(first: 1) { nodes { id } }
                                    labels(first: 20) { nodes { name } }
                                }
                            }
                        }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        }
    "#;

    let variables = serde_json::json!({
        "projectId": project_node_id,
        "cursor": cursor,
    });

    let mut resp = execute_github_graphql(client, token, graphql_url, query, variables).await?;

    let has_next = resp["data"]["node"]["items"]["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false);
    let end_cursor = resp["data"]["node"]["items"]["pageInfo"]["endCursor"]
        .as_str()
        .map(std::string::ToString::to_string);

    // Take ownership of the nodes array in-place instead of deep-cloning it.
    let nodes = resp
        .pointer_mut("/data/node/items/nodes")
        .and_then(|v| v.as_array_mut())
        .map(std::mem::take)
        .unwrap_or_default();

    Ok((nodes, has_next, end_cursor))
}

#[async_trait]
impl Tracker for GitHubTracker {
    async fn fetch_viewer_id(&self) -> Result<String, String> {
        tracing::debug!("Fetching viewer ID from GitHub");
        let token = self.fresh_token().await?;
        let query = "query { viewer { id } }";
        let resp = execute_github_graphql(
            &self.client,
            &token,
            &self.graphql_url(),
            query,
            serde_json::json!({}),
        )
        .await?;

        resp["data"]["viewer"]["id"]
            .as_str()
            .map(std::string::ToString::to_string)
            .ok_or_else(|| "Missing viewer id in GitHub response".to_string())
    }

    async fn create_comment(&self, issue: &Issue, body: &str) -> Result<(), String> {
        tracing::debug!(issue_id = %issue.id, "Creating comment on GitHub issue");
        let token = self.fresh_token().await?;
        let query = r"
            mutation($subjectId: ID!, $body: String!) {
                addComment(input: { subjectId: $subjectId, body: $body }) {
                    clientMutationId
                }
            }
        ";
        let variables = serde_json::json!({
            "subjectId": issue.id,
            "body": body,
        });
        execute_github_graphql(&self.client, &token, &self.graphql_url(), query, variables).await?;
        Ok(())
    }

    async fn update_issue_state(&self, issue: &Issue, state_name: &str) -> Result<(), String> {
        let project_item_id = issue
            .project_item_id
            .as_deref()
            .ok_or("update_issue_state requires project_item_id")?;

        tracing::debug!(
            project_item_id,
            state_name,
            "Updating GitHub project item status"
        );

        let token = self.fresh_token().await?;
        let project_node_id = self.resolve_project_node_id(&token).await?;
        let graphql_url = self.graphql_url();

        // Step 1: Get the Status field ID and the target option ID
        let field_query = r#"
            query($projectId: ID!) {
                node(id: $projectId) {
                    ... on ProjectV2 {
                        field(name: "Status") {
                            ... on ProjectV2SingleSelectField {
                                id
                                options { id name }
                            }
                        }
                    }
                }
            }
        "#;
        let field_resp = execute_github_graphql(
            &self.client,
            &token,
            &graphql_url,
            field_query,
            serde_json::json!({ "projectId": project_node_id }),
        )
        .await?;

        let field = &field_resp["data"]["node"]["field"];
        let field_id = field["id"]
            .as_str()
            .ok_or("Missing Status field id")?
            .to_string();

        let option_id = field["options"]
            .as_array()
            .and_then(|opts| {
                opts.iter().find(|o| {
                    o["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(state_name))
                })
            })
            .and_then(|o| o["id"].as_str())
            .ok_or_else(|| format!("Status option '{state_name}' not found in project"))?
            .to_string();

        // Step 2: Update the field value
        let update_query = r"
            mutation($projectId: ID!, $itemId: ID!, $fieldId: ID!, $optionId: String!) {
                updateProjectV2ItemFieldValue(input: {
                    projectId: $projectId
                    itemId: $itemId
                    fieldId: $fieldId
                    value: { singleSelectOptionId: $optionId }
                }) {
                    projectV2Item { id }
                }
            }
        ";
        execute_github_graphql(
            &self.client,
            &token,
            &graphql_url,
            update_query,
            serde_json::json!({
                "projectId": project_node_id,
                "itemId": project_item_id,
                "fieldId": field_id,
                "optionId": option_id,
            }),
        )
        .await?;

        Ok(())
    }

    async fn fetch_candidate_issues(&self, state_names: &[&str]) -> Result<Vec<Issue>, String> {
        tracing::debug!(
            owner = %self.owner,
            project_number = self.project_number,
            ?state_names,
            "Fetching candidate issues from GitHub project"
        );

        let token = self.fresh_token().await?;
        let project_node_id = self.resolve_project_node_id(&token).await?;
        let graphql_url = self.graphql_url();

        let mut all_issues = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let (nodes, has_next, end_cursor) = fetch_project_items_page(
                &self.client,
                &token,
                &graphql_url,
                project_node_id,
                cursor.as_deref(),
            )
            .await?;

            for node in &nodes {
                if let Some(issue) = normalize_github_item(node) {
                    if state_names
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&issue.state))
                    {
                        all_issues.push(issue);
                    }
                }
            }

            if has_next {
                cursor = end_cursor;
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

        tracing::debug!(
            count = ids.len(),
            "Fetching GitHub issues by ID from project"
        );

        let token = self.fresh_token().await?;
        let project_node_id = self.resolve_project_node_id(&token).await?;
        let graphql_url = self.graphql_url();

        let id_set: std::collections::HashSet<&str> = ids.iter().copied().collect();
        let mut issue_map: std::collections::HashMap<String, Issue> =
            std::collections::HashMap::with_capacity(ids.len());
        let mut cursor: Option<String> = None;

        loop {
            let (nodes, has_next, end_cursor) = fetch_project_items_page(
                &self.client,
                &token,
                &graphql_url,
                project_node_id,
                cursor.as_deref(),
            )
            .await?;

            for node in &nodes {
                if let Some(issue) = normalize_github_item(node) {
                    if id_set.contains(issue.id.as_str()) {
                        issue_map.insert(issue.id.clone(), issue);
                    }
                }
            }

            if issue_map.len() == id_set.len() {
                break;
            }

            if has_next {
                cursor = end_cursor;
            } else {
                break;
            }
        }

        // Return in the same order as the input IDs
        Ok(ids.iter().filter_map(|id| issue_map.remove(*id)).collect())
    }
}

#[cfg(test)]
mod tests {
    use fabro_github::GitHubAppCredentials;

    use super::*;
    use crate::Issue;
    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn test_rsa_key() -> String {
        include_str!("fixtures/github-app-test-key.pem").to_string()
    }

    // -----------------------------------------------------------------------
    // execute_github_graphql
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_github_graphql_success() {
        let server = httpmock::MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .header("User-Agent", "fabro");
                then.status(200)
                    .body(r#"{"data": {"viewer": {"id": "U_abc"}}}"#);
            })
            .await;

        let client = test_http_client();
        let result = execute_github_graphql(
            &client,
            "test-token",
            &server.url("/graphql"),
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        assert_eq!(result["data"]["viewer"]["id"], "U_abc");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn execute_github_graphql_http_error() {
        let server = httpmock::MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method("POST").path("/graphql");
                then.status(401).body("Unauthorized");
            })
            .await;

        let client = test_http_client();
        let err = execute_github_graphql(
            &client,
            "bad-token",
            &server.url("/graphql"),
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(err.contains("401"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_github_graphql_errors_array() {
        let server = httpmock::MockServer::start_async().await;

        server
            .mock_async(|when, then| {
                when.method("POST").path("/graphql");
                then.status(200)
                    .body(r#"{"data": null, "errors": [{"message": "Not found"}]}"#);
            })
            .await;

        let client = test_http_client();
        let err = execute_github_graphql(
            &client,
            "token",
            &server.url("/graphql"),
            "query { bad }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Not found"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_github_graphql_correct_headers() {
        let server = httpmock::MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .header("Authorization", "Bearer my-token")
                    .header("Content-Type", "application/json")
                    .header("User-Agent", "fabro");
                then.status(200).body(r#"{"data": {}}"#);
            })
            .await;

        let client = test_http_client();
        execute_github_graphql(
            &client,
            "my-token",
            &server.url("/graphql"),
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        mock.assert_async().await;
    }

    // -----------------------------------------------------------------------
    // GitHubTracker helpers
    // -----------------------------------------------------------------------

    fn mock_github_tracker(server_url: &str, pem: String) -> GitHubTracker {
        GitHubTracker::new(
            GitHubAppCredentials {
                app_id: "test-app".to_string(),
                private_key_pem: pem,
            },
            test_http_client(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            server_url.to_string(),
        )
    }

    fn make_test_issue(state: &str) -> Issue {
        Issue {
            id: "I_issue1".to_string(),
            project_item_id: Some("PVTI_item1".to_string()),
            identifier: "#42".to_string(),
            title: "Fix bug".to_string(),
            description: None,
            priority: None,
            state: state.to_string(),
            branch_name: None,
            url: "https://github.com/owner/repo/issues/42".to_string(),
            assignee_id: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    fn org_project_node_id_response() -> &'static str {
        r#"{"data": {"organization": {"projectV2": {"id": "PVT_abc123"}}}}"#
    }

    fn empty_items_response() -> &'static str {
        r#"{"data": {"node": {"items": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}}}}"#
    }

    fn single_item_response(status: &str) -> String {
        serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [
                            {
                                "id": "PVTI_item1",
                                "fieldValueByName": {"name": status},
                                "content": {
                                    "id": "I_issue1",
                                    "number": 42,
                                    "title": "Fix bug",
                                    "body": "Description",
                                    "url": "https://github.com/owner/repo/issues/42",
                                    "createdAt": "2026-01-01T00:00:00Z",
                                    "updatedAt": "2026-01-02T00:00:00Z",
                                    "assignees": {"nodes": []},
                                    "labels": {"nodes": [{"name": "bug"}]}
                                }
                            }
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
        .to_string()
    }

    // -----------------------------------------------------------------------
    // project node ID resolution
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn project_node_id_resolved_via_org() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("items(first:");
                then.status(200).body(empty_items_response());
            })
            .await;

        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn project_node_id_falls_back_to_user() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(r#"{"data": {"organization": null}}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("user(login:");
                then.status(200)
                    .body(r#"{"data": {"user": {"projectV2": {"id": "PVT_user1"}}}}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("items(first:");
                then.status(200).body(empty_items_response());
            })
            .await;

        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();
        assert!(issues.is_empty());
    }

    // -----------------------------------------------------------------------
    // fetch_viewer_id (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_fetch_viewer_id_success() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/graphql");
                then.status(200)
                    .body(r#"{"data": {"viewer": {"id": "U_xyz"}}}"#);
            })
            .await;

        let id = tracker.fetch_viewer_id().await.unwrap();
        assert_eq!(id, "U_xyz");
    }

    // -----------------------------------------------------------------------
    // create_comment (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_create_comment_success() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/graphql");
                then.status(200)
                    .body(r#"{"data": {"addComment": {"clientMutationId": null}}}"#);
            })
            .await;

        let issue = make_test_issue("In Progress");
        tracker.create_comment(&issue, "Great work!").await.unwrap();
    }

    // -----------------------------------------------------------------------
    // update_issue_state (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_update_issue_state_success() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        // Field query
        server.mock_async(|when, then| {
            when.method("POST").path("/graphql").body_includes("field(name:");
            then.status(200).body(r#"{"data": {"node": {"field": {"id": "FLD_1", "options": [{"id": "opt-done", "name": "Done"}, {"id": "opt-todo", "name": "Todo"}]}}}}"#);
        }).await;
        server.mock_async(|when, then| {
            when.method("POST").path("/graphql").body_includes("updateProjectV2ItemFieldValue");
            then.status(200).body(r#"{"data": {"updateProjectV2ItemFieldValue": {"projectV2Item": {"id": "PVTI_item1"}}}}"#);
        }).await;

        let issue = make_test_issue("In Progress");
        tracker.update_issue_state(&issue, "Done").await.unwrap();
    }

    #[tokio::test]
    async fn github_tracker_update_issue_state_status_not_found() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        // Field query — options don't include "Nonexistent"
        server.mock_async(|when, then| {
            when.method("POST").path("/graphql").body_includes("field(name:");
            then.status(200).body(r#"{"data": {"node": {"field": {"id": "FLD_1", "options": [{"id": "opt-done", "name": "Done"}]}}}}"#);
        }).await;

        let issue = make_test_issue("Todo");
        let err = tracker
            .update_issue_state(&issue, "Nonexistent")
            .await
            .unwrap_err();
        assert!(err.contains("Nonexistent"), "got: {err}");
        assert!(err.contains("not found"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // fetch_candidate_issues (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_fetch_candidate_issues_single_page() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("items(first:");
                then.status(200).body(single_item_response("In Progress"));
            })
            .await;

        let issues = tracker
            .fetch_candidate_issues(&["In Progress"])
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "#42");
        assert_eq!(issues[0].state, "In Progress");
        assert_eq!(issues[0].id, "I_issue1");
        assert_eq!(issues[0].project_item_id.as_deref(), Some("PVTI_item1"));
        assert_eq!(issues[0].labels, vec!["bug"]);
        assert!(issues[0].branch_name.is_none());
        assert!(issues[0].priority.is_none());
    }

    #[tokio::test]
    async fn github_tracker_fetch_candidate_issues_empty() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("items(first:");
                then.status(200).body(empty_items_response());
            })
            .await;

        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn github_tracker_fetch_candidate_issues_status_filtering() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        let items_body = serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [
                            {
                                "id": "PVTI_done",
                                "fieldValueByName": {"name": "Done"},
                                "content": {
                                    "id": "I_done1", "number": 10, "title": "Done issue",
                                    "body": null, "url": "https://github.com/owner/repo/issues/10",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            },
                            {
                                "id": "PVTI_inprog",
                                "fieldValueByName": {"name": "In Progress"},
                                "content": {
                                    "id": "I_inprog1", "number": 20, "title": "Active issue",
                                    "body": null, "url": "https://github.com/owner/repo/issues/20",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            }
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
        .to_string();

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("items(first:");
                then.status(200).body(items_body);
            })
            .await;

        let issues = tracker
            .fetch_candidate_issues(&["In Progress"])
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "#20");
    }

    // -----------------------------------------------------------------------
    // fetch_issues_by_ids (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_fetch_issues_by_ids_ordering() {
        let server = httpmock::MockServer::start_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(""), pem);

        // Page returns issues in reverse order of what we request
        let items_body = serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [
                            {
                                "id": "PVTI_b",
                                "fieldValueByName": {"name": "Todo"},
                                "content": {
                                    "id": "I_b", "number": 2, "title": "B",
                                    "body": null, "url": "https://github.com/owner/repo/issues/2",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            },
                            {
                                "id": "PVTI_a",
                                "fieldValueByName": {"name": "Todo"},
                                "content": {
                                    "id": "I_a", "number": 1, "title": "A",
                                    "body": null, "url": "https://github.com/owner/repo/issues/1",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            }
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
        .to_string();

        server
            .mock_async(|when, then| {
                when.method("GET").path("/repos/owner/repo/installation");
                then.status(200).body(r#"{"id": 1}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/app/installations/1/access_tokens");
                then.status(201).body(r#"{"token": "ghs_test"}"#);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("organization(login:");
                then.status(200).body(org_project_node_id_response());
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/graphql")
                    .body_includes("items(first:");
                then.status(200).body(items_body);
            })
            .await;

        // Request in A, B order — should get back in A, B order despite page returning
        // B, A
        let issues = tracker.fetch_issues_by_ids(&["I_a", "I_b"]).await.unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "I_a");
        assert_eq!(issues[1].id, "I_b");
    }

    #[tokio::test]
    async fn github_tracker_fetch_issues_by_ids_empty() {
        let pem = test_rsa_key();
        let tracker = mock_github_tracker("http://unused", pem);

        // Empty input → no HTTP calls at all
        let issues = tracker.fetch_issues_by_ids(&[]).await.unwrap();
        assert!(issues.is_empty());
    }
}
