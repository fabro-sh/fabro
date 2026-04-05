use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::auth::{
    BearerTokenError, GraphqlActor, authorize_graphql_actor, ensure_permission,
    ensure_repo_permission,
};
use crate::server::SharedState;
use crate::state::{AutoMerge, Comment, OwnerType, PermissionLevel, TokenPermission};

fn graphql_auth_error_response(error: BearerTokenError) -> axum::response::Response {
    match error {
        BearerTokenError::Missing => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"message": "Unauthorized"})),
        )
            .into_response(),
        BearerTokenError::Invalid => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"message": "Bad credentials"})),
        )
            .into_response(),
    }
}

fn graphql_permission_error_response() -> axum::response::Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "errors": [{"message": "Resource not accessible by integration"}]
        })),
    )
        .into_response()
}

fn graphql_project_lookup_inaccessible_response(number: u64) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "organization": {
                    "projectV2": null
                }
            },
            "errors": [{
                "message": format!("Could not resolve to a ProjectV2 with the number {}.", number)
            }]
        })),
    )
        .into_response()
}

/// POST /graphql — handles all GraphQL operations via pattern matching
pub async fn handle_graphql(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let actor = {
        let state = state.read().await;
        match authorize_graphql_actor(&headers, &state) {
            Ok(actor) => actor,
            Err(error) => return graphql_auth_error_response(error),
        }
    };

    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let variables = body
        .get("variables")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // Pattern match on query content
    if query.contains("viewer") && query.contains("id") && !query.contains("mutation") {
        return handle_viewer_query(state).await;
    }

    if query.contains("enablePullRequestAutoMerge") {
        return handle_enable_auto_merge(state, &actor, query).await;
    }

    if query.contains("addComment") {
        return handle_add_comment(state, &actor, &variables).await;
    }

    if query.contains("updateProjectV2ItemFieldValue") {
        return handle_update_project_item(state, &actor, &variables).await;
    }

    if query.contains("organization") && query.contains("projectV2") {
        return handle_org_project_query(state, &actor, &variables).await;
    }

    if query.contains("user") && query.contains("projectV2") && !query.contains("organization") {
        return handle_user_project_query(state, &actor, &variables).await;
    }

    if query.contains("node") && query.contains("items") {
        return handle_project_items_query(state, &actor, &variables).await;
    }

    if query.contains("field") && query.contains("Status") {
        return handle_project_field_query(state, &actor, &variables).await;
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "errors": [{"message": "Unsupported query"}]
        })),
    )
        .into_response()
}

async fn handle_viewer_query(state: SharedState) -> axum::response::Response {
    let state = state.read().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "viewer": {
                    "id": state.viewer_id
                }
            }
        })),
    )
        .into_response()
}

async fn handle_enable_auto_merge(
    state: SharedState,
    actor: &GraphqlActor,
    query: &str,
) -> axum::response::Response {
    // Extract pullRequestId from the query string (inline format)
    let pr_id = extract_quoted_value(query, "pullRequestId:");
    let merge_method = extract_unquoted_value(query, "mergeMethod:");

    if let Some(pr_id) = pr_id {
        let mut state = state.write().await;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let method = merge_method.unwrap_or_else(|| "SQUASH".to_string());

        // Find the PR by node_id and update auto_merge
        for ((_, repo), prs) in &mut state.pull_requests {
            for pr in prs.iter_mut() {
                if pr.node_id == pr_id {
                    if let GraphqlActor::InstallationToken(token) = actor {
                        if ensure_repo_permission(
                            token,
                            repo,
                            TokenPermission::PullRequests,
                            PermissionLevel::Write,
                        )
                        .is_err()
                        {
                            return graphql_permission_error_response();
                        }
                    }
                    pr.auto_merge = Some(AutoMerge {
                        enabled_at: now.clone(),
                        merge_method: method.clone(),
                    });
                    return (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "data": {
                                "enablePullRequestAutoMerge": {
                                    "pullRequest": {
                                        "autoMergeRequest": {
                                            "enabledAt": now,
                                            "mergeMethod": method,
                                        }
                                    }
                                }
                            }
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "errors": [{"message": "Pull request not found"}]
        })),
    )
        .into_response()
}

async fn handle_add_comment(
    state: SharedState,
    actor: &GraphqlActor,
    variables: &serde_json::Value,
) -> axum::response::Response {
    let subject_id = variables
        .get("subjectId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = variables
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut state = state.write().await;
    if let GraphqlActor::InstallationToken(token) = actor {
        for ((_, repo), prs) in &state.pull_requests {
            if prs.iter().any(|pr| pr.node_id == subject_id) && !token.allows_repo(repo) {
                return graphql_permission_error_response();
            }
        }
    }
    state.comments.push(Comment {
        issue_node_id: subject_id,
        body,
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "addComment": {
                    "clientMutationId": null
                }
            }
        })),
    )
        .into_response()
}

async fn handle_org_project_query(
    state: SharedState,
    actor: &GraphqlActor,
    variables: &serde_json::Value,
) -> axum::response::Response {
    let owner = variables
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let number = variables
        .get("number")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let state = state.read().await;
    if let GraphqlActor::InstallationToken(token) = actor {
        if ensure_permission(
            token,
            TokenPermission::OrganizationProjects,
            PermissionLevel::Read,
        )
        .is_err()
        {
            return graphql_project_lookup_inaccessible_response(number);
        }
    }
    for project in &state.projects {
        if project.owner == owner
            && project.number == number
            && project.owner_type == OwnerType::Organization
        {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "data": {
                        "organization": {
                            "projectV2": {
                                "id": project.node_id
                            }
                        }
                    }
                })),
            )
                .into_response();
        }
    }

    // Return null projectV2 (not an error — the caller handles the fallback to user query)
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "organization": {
                    "projectV2": null
                }
            }
        })),
    )
        .into_response()
}

async fn handle_user_project_query(
    state: SharedState,
    _actor: &GraphqlActor,
    variables: &serde_json::Value,
) -> axum::response::Response {
    let owner = variables
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let number = variables
        .get("number")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let state = state.read().await;
    for project in &state.projects {
        if project.owner == owner
            && project.number == number
            && project.owner_type == OwnerType::User
        {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "data": {
                        "user": {
                            "projectV2": {
                                "id": project.node_id
                            }
                        }
                    }
                })),
            )
                .into_response();
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "user": {
                    "projectV2": null
                }
            }
        })),
    )
        .into_response()
}

async fn handle_project_items_query(
    state: SharedState,
    actor: &GraphqlActor,
    variables: &serde_json::Value,
) -> axum::response::Response {
    let project_id = variables
        .get("projectId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let state = state.read().await;
    for project in &state.projects {
        if project.node_id == project_id {
            if project.owner_type == OwnerType::Organization {
                if let GraphqlActor::InstallationToken(token) = actor {
                    if ensure_permission(
                        token,
                        TokenPermission::OrganizationProjects,
                        PermissionLevel::Read,
                    )
                    .is_err()
                    {
                        return graphql_permission_error_response();
                    }
                }
            }
            let nodes: Vec<serde_json::Value> = project
                .items
                .iter()
                .map(|item| {
                    let assignee_nodes: Vec<serde_json::Value> = item
                        .content
                        .assignee_ids
                        .iter()
                        .map(|id| serde_json::json!({"id": id}))
                        .collect();
                    let label_nodes: Vec<serde_json::Value> = item
                        .content
                        .labels
                        .iter()
                        .map(|name| serde_json::json!({"name": name}))
                        .collect();

                    serde_json::json!({
                        "id": item.id,
                        "fieldValueByName": {
                            "name": item.status
                        },
                        "content": {
                            "id": item.content.id,
                            "number": item.content.number,
                            "title": item.content.title,
                            "body": item.content.body,
                            "url": item.content.url,
                            "createdAt": item.content.created_at,
                            "updatedAt": item.content.updated_at,
                            "assignees": {
                                "nodes": assignee_nodes
                            },
                            "labels": {
                                "nodes": label_nodes
                            }
                        }
                    })
                })
                .collect();

            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "data": {
                        "node": {
                            "items": {
                                "nodes": nodes,
                                "pageInfo": {
                                    "hasNextPage": false,
                                    "endCursor": null
                                }
                            }
                        }
                    }
                })),
            )
                .into_response();
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            }
        })),
    )
        .into_response()
}

async fn handle_project_field_query(
    state: SharedState,
    actor: &GraphqlActor,
    variables: &serde_json::Value,
) -> axum::response::Response {
    let project_id = variables
        .get("projectId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let state = state.read().await;
    for project in &state.projects {
        if project.node_id == project_id {
            if project.owner_type == OwnerType::Organization {
                if let GraphqlActor::InstallationToken(token) = actor {
                    if ensure_permission(
                        token,
                        TokenPermission::OrganizationProjects,
                        PermissionLevel::Read,
                    )
                    .is_err()
                    {
                        return graphql_permission_error_response();
                    }
                }
            }
            let options: Vec<serde_json::Value> = project
                .status_options
                .iter()
                .map(|opt| serde_json::json!({"id": opt.id, "name": opt.name}))
                .collect();

            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "data": {
                        "node": {
                            "field": {
                                "id": project.status_field_id,
                                "options": options
                            }
                        }
                    }
                })),
            )
                .into_response();
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "errors": [{"message": "Project not found"}]
        })),
    )
        .into_response()
}

async fn handle_update_project_item(
    state: SharedState,
    actor: &GraphqlActor,
    variables: &serde_json::Value,
) -> axum::response::Response {
    let project_id = variables
        .get("projectId")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let item_id = variables
        .get("itemId")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let option_id = variables
        .get("optionId")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut state = state.write().await;
    for project in &mut state.projects {
        if project.node_id == project_id {
            if project.owner_type == OwnerType::Organization {
                if let GraphqlActor::InstallationToken(token) = actor {
                    if ensure_permission(
                        token,
                        TokenPermission::OrganizationProjects,
                        PermissionLevel::Write,
                    )
                    .is_err()
                    {
                        return graphql_permission_error_response();
                    }
                }
            }
            // Find the option name by ID
            let option_name = project
                .status_options
                .iter()
                .find(|o| o.id == option_id)
                .map(|o| o.name.clone());

            if let Some(name) = option_name {
                for item in &mut project.items {
                    if item.id == item_id {
                        item.status = name;
                        return (
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "data": {
                                    "updateProjectV2ItemFieldValue": {
                                        "projectV2Item": {
                                            "id": item_id
                                        }
                                    }
                                }
                            })),
                        )
                            .into_response();
                    }
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "errors": [{"message": "Item not found"}]
        })),
    )
        .into_response()
}

/// Extract a quoted value after a key in a string, e.g. `pullRequestId: "PR_123"` -> `PR_123`
fn extract_quoted_value(s: &str, key: &str) -> Option<String> {
    let idx = s.find(key)?;
    let rest = &s[idx + key.len()..];
    let rest = rest.trim_start();
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        None
    }
}

/// Extract an unquoted value after a key, e.g. `mergeMethod: SQUASH` -> `SQUASH`
fn extract_unquoted_value(s: &str, key: &str) -> Option<String> {
    let idx = s.find(key)?;
    let rest = &s[idx + key.len()..];
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    if end > 0 {
        Some(rest[..end].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::server::TestServer;
    use crate::state::{AppOptions, AppState, PullRequest};
    use crate::test_support::{sign_test_jwt, test_rsa_private_key};

    async fn get_installation_token(
        client: &reqwest::Client,
        jwt: &str,
        owner: &str,
        repo: &str,
        base_url: &str,
    ) -> String {
        // Step 1: GET /repos/{owner}/{repo}/installation to get installation ID
        let resp = client
            .get(&format!("{base_url}/repos/{owner}/{repo}/installation"))
            .header("Authorization", format!("Bearer {jwt}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let install_id = body["id"].as_u64().unwrap();

        // Step 2: POST /app/installations/{id}/access_tokens
        let resp = client
            .post(&format!(
                "{base_url}/app/installations/{install_id}/access_tokens"
            ))
            .header("Authorization", format!("Bearer {jwt}"))
            .json(&serde_json::json!({
                "repositories": [repo],
                "permissions": {
                    "contents": "write",
                    "pull_requests": "write",
                    "issues": "write",
                    "organization_projects": "write"
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        body["token"].as_str().unwrap().to_string()
    }

    async fn setup_with_token(
        state: &mut AppState,
        pem: &str,
    ) -> (TestServer, reqwest::Client, String) {
        state.register_app(AppOptions {
            app_id: "100".to_string(),
            slug: "test-app".to_string(),
            owner_login: "owner".to_string(),
            public: true,
            private_key_pem: pem.to_string(),
            webhook_secret: None,
        });
        state.add_installation("100", "owner", vec!["repo".to_string()], false);
        state.add_repository("owner", "repo", vec!["main".to_string()], false);
        let server = TestServer::start(state.clone()).await;

        let jwt = sign_test_jwt("100", pem);
        let client = reqwest::Client::new();
        let token = get_installation_token(&client, &jwt, "owner", "repo", server.url()).await;

        (server, client, token)
    }

    #[tokio::test]
    async fn viewer_query_returns_id() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.viewer_id = "U_testviewer".to_string();
        let (server, client, token) = setup_with_token(&mut state, pem).await;

        let resp = client
            .post(&format!("{}/graphql", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "query": "query { viewer { id } }",
                "variables": {}
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"]["viewer"]["id"], "U_testviewer");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn enable_auto_merge_mutation() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        // Pre-seed a PR
        state
            .pull_requests
            .entry(("owner".to_string(), "repo".to_string()))
            .or_default()
            .push(PullRequest {
                number: 1,
                node_id: "PR_test123".to_string(),
                title: "Test".to_string(),
                body: "".to_string(),
                state: "open".to_string(),
                draft: false,
                mergeable: true,
                additions: 10,
                deletions: 5,
                changed_files: 2,
                html_url: "https://github.com/owner/repo/pull/1".to_string(),
                user_login: "test-bot[bot]".to_string(),
                head_ref: "feature".to_string(),
                base_ref: "main".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                auto_merge: None,
            });
        let (server, client, token) = setup_with_token(&mut state, pem).await;

        let query = r#"mutation {
            enablePullRequestAutoMerge(input: {pullRequestId: "PR_test123", mergeMethod: SQUASH}) {
                pullRequest {
                    autoMergeRequest {
                        enabledAt
                        mergeMethod
                    }
                }
            }
        }"#;

        let resp = client
            .post(&format!("{}/graphql", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["data"]["enablePullRequestAutoMerge"]["pullRequest"]["autoMergeRequest"]
                ["enabledAt"]
                .is_string()
        );
        assert!(body["errors"].is_null());

        server.shutdown().await;
    }

    #[tokio::test]
    async fn graphql_unauthorized_returns_401() {
        let state = AppState::new();
        let server = TestServer::start(state).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(&format!("{}/graphql", server.url()))
            .header("Authorization", "Bearer invalid-token")
            .json(&serde_json::json!({
                "query": "query { viewer { id } }",
                "variables": {}
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn org_project_query_returns_node_id() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.projects.push(crate::state::Project {
            node_id: "PVT_org123".to_string(),
            number: 1,
            owner: "owner".to_string(),
            owner_type: crate::state::OwnerType::Organization,
            status_field_id: "PVTSSF_status1".to_string(),
            status_options: vec![
                crate::state::StatusOption {
                    id: "opt1".to_string(),
                    name: "Todo".to_string(),
                },
                crate::state::StatusOption {
                    id: "opt2".to_string(),
                    name: "In Progress".to_string(),
                },
                crate::state::StatusOption {
                    id: "opt3".to_string(),
                    name: "Done".to_string(),
                },
            ],
            items: vec![crate::state::ProjectItem {
                id: "PVTI_item1".to_string(),
                status: "Todo".to_string(),
                content: crate::state::IssueContent {
                    id: "I_issue1".to_string(),
                    number: 42,
                    title: "Fix bug".to_string(),
                    body: "Description".to_string(),
                    url: "https://github.com/owner/repo/issues/42".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-02T00:00:00Z".to_string(),
                    assignee_ids: vec![],
                    labels: vec!["bug".to_string()],
                },
            }],
        });
        let (server, client, token) = setup_with_token(&mut state, pem).await;

        // Query org project
        let query = r#"
            query($owner: String!, $number: Int!) {
                organization(login: $owner) {
                    projectV2(number: $number) { id }
                }
            }
        "#;
        let resp = client
            .post(&format!("{}/graphql", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "query": query,
                "variables": { "owner": "owner", "number": 1 }
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["data"]["organization"]["projectV2"]["id"],
            "PVT_org123"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn project_items_query_returns_paginated() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.projects.push(crate::state::Project {
            node_id: "PVT_test".to_string(),
            number: 1,
            owner: "owner".to_string(),
            owner_type: crate::state::OwnerType::Organization,
            status_field_id: "PVTSSF_s".to_string(),
            status_options: vec![],
            items: vec![crate::state::ProjectItem {
                id: "PVTI_1".to_string(),
                status: "Todo".to_string(),
                content: crate::state::IssueContent {
                    id: "I_1".to_string(),
                    number: 1,
                    title: "Issue 1".to_string(),
                    body: "Body".to_string(),
                    url: "https://github.com/owner/repo/issues/1".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-01T00:00:00Z".to_string(),
                    assignee_ids: vec!["U_user1".to_string()],
                    labels: vec!["bug".to_string(), "urgent".to_string()],
                },
            }],
        });
        let (server, client, token) = setup_with_token(&mut state, pem).await;

        let query = r#"
            query($projectId: ID!, $cursor: String) {
                node(id: $projectId) {
                    ... on ProjectV2 {
                        items(first: 100, after: $cursor) {
                            nodes {
                                id
                                fieldValueByName(name: "Status") {
                                    ... on ProjectV2ItemFieldSingleSelectValue { name }
                                }
                                content {
                                    ... on Issue {
                                        id number title body url createdAt updatedAt
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
        let resp = client
            .post(&format!("{}/graphql", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "query": query,
                "variables": { "projectId": "PVT_test", "cursor": null }
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let items = &body["data"]["node"]["items"]["nodes"];
        assert_eq!(items.as_array().unwrap().len(), 1);
        assert_eq!(items[0]["id"], "PVTI_1");
        assert_eq!(items[0]["fieldValueByName"]["name"], "Todo");
        assert_eq!(items[0]["content"]["number"], 1);
        assert_eq!(
            items[0]["content"]["assignees"]["nodes"][0]["id"],
            "U_user1"
        );
        assert_eq!(items[0]["content"]["labels"]["nodes"][0]["name"], "bug");
        assert_eq!(
            body["data"]["node"]["items"]["pageInfo"]["hasNextPage"],
            false
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn update_project_item_field_value() {
        let pem = test_rsa_private_key();
        let mut state = AppState::new();
        state.projects.push(crate::state::Project {
            node_id: "PVT_test".to_string(),
            number: 1,
            owner: "owner".to_string(),
            owner_type: crate::state::OwnerType::Organization,
            status_field_id: "PVTSSF_s".to_string(),
            status_options: vec![
                crate::state::StatusOption {
                    id: "opt1".to_string(),
                    name: "Todo".to_string(),
                },
                crate::state::StatusOption {
                    id: "opt2".to_string(),
                    name: "Done".to_string(),
                },
            ],
            items: vec![crate::state::ProjectItem {
                id: "PVTI_1".to_string(),
                status: "Todo".to_string(),
                content: crate::state::IssueContent {
                    id: "I_1".to_string(),
                    number: 1,
                    title: "Issue 1".to_string(),
                    body: "".to_string(),
                    url: "https://github.com/owner/repo/issues/1".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-01T00:00:00Z".to_string(),
                    assignee_ids: vec![],
                    labels: vec![],
                },
            }],
        });
        let (server, client, token) = setup_with_token(&mut state, pem).await;

        let query = r#"
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
        "#;
        let resp = client
            .post(&format!("{}/graphql", server.url()))
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "query": query,
                "variables": {
                    "projectId": "PVT_test",
                    "itemId": "PVTI_1",
                    "fieldId": "PVTSSF_s",
                    "optionId": "opt2"
                }
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["data"]["updateProjectV2ItemFieldValue"]["projectV2Item"]["id"],
            "PVTI_1"
        );
        server.shutdown().await;
    }
}
