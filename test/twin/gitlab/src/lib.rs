mod api;
mod oauth;
mod server;
mod state;

pub use server::TestGitLabServer;
pub use state::{GitLabGroupFixture, GitLabProjectFixture, GitLabUserFixture};

#[cfg(test)]
mod tests {
    use super::TestGitLabServer;

    #[expect(
        clippy::disallowed_methods,
        reason = "This twin contract test must explicitly prove local reqwest clients disable proxy discovery."
    )]
    #[tokio::test]
    async fn creates_fetches_merges_and_closes_merge_request() {
        let server = TestGitLabServer::start().await;
        server.add_user("alice", "Alice Example", "alice@example.test");
        server.add_project("platform/tools/fabro", "main", &["feature/fabro"]);

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = server.base_url();
        let create_url = format!("{base}/api/v4/projects/platform%2Ftools%2Ffabro/merge_requests");
        let response = client
            .post(create_url)
            .bearer_auth(server.automation_token())
            .json(&serde_json::json!({
                "source_branch": "feature/fabro",
                "target_branch": "main",
                "title": "Implement GitLab",
                "description": "Body",
                "remove_source_branch": false
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["iid"], 1);
        assert_eq!(
            value["web_url"],
            format!("{base}/platform/tools/fabro/-/merge_requests/1")
        );

        let get_url = format!("{base}/api/v4/projects/platform%2Ftools%2Ffabro/merge_requests/1");
        assert_eq!(
            client
                .get(&get_url)
                .bearer_auth(server.automation_token())
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );

        assert_eq!(
            client
                .put(format!("{get_url}/merge"))
                .bearer_auth(server.automation_token())
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );

        assert_eq!(
            client
                .put(get_url)
                .bearer_auth(server.automation_token())
                .json(&serde_json::json!({ "state_event": "close" }))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "This twin contract test must explicitly prove local reqwest clients disable proxy discovery."
    )]
    #[tokio::test]
    async fn fetches_project_by_encoded_path() {
        let server = TestGitLabServer::start().await;
        server.add_project("platform/tools/fabro", "main", &["feature/fabro"]);

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!(
                "{}/api/v4/projects/platform%2Ftools%2Ffabro",
                server.base_url()
            ))
            .bearer_auth(server.automation_token())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["path_with_namespace"], "platform/tools/fabro");
        assert_eq!(value["default_branch"], "main");
        assert_eq!(
            value["web_url"],
            format!("{}/platform/tools/fabro", server.base_url())
        );
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "This twin contract test must explicitly prove local reqwest clients disable proxy discovery."
    )]
    #[tokio::test]
    async fn fetches_project_by_numeric_id() {
        let server = TestGitLabServer::start().await;
        server.add_project_with_id(42, "platform/tools/fabro", "main", &["feature/fabro"]);

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!("{}/api/v4/projects/42", server.base_url()))
            .bearer_auth(server.automation_token())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["id"], 42);
        assert_eq!(value["path_with_namespace"], "platform/tools/fabro");
        assert_eq!(value["default_branch"], "main");
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "This twin contract test must explicitly prove local reqwest clients disable proxy discovery."
    )]
    #[tokio::test]
    async fn searches_projects_with_offset_pagination() {
        let server = TestGitLabServer::start().await;
        server.add_project_with_id(10, "acme/tools/fabro-helper", "main", &[]);
        server.add_project_with_id(42, "platform/tools/fabro", "main", &[]);
        server.add_project_with_id(99, "platform/archive/other", "main", &[]);

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!("{}/api/v4/projects", server.base_url()))
            .bearer_auth(server.automation_token())
            .query(&[
                ("simple", "true"),
                ("per_page", "1"),
                ("page", "2"),
                ("search", "fabro"),
            ])
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let values: Vec<serde_json::Value> = response.json().await.unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["id"], 42);
        assert_eq!(values[0]["path_with_namespace"], "platform/tools/fabro");
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "This twin contract test must explicitly prove local reqwest clients disable proxy discovery."
    )]
    #[tokio::test]
    async fn rejects_user_and_group_requests_without_automation_token() {
        let server = TestGitLabServer::start().await;
        server.add_user("alice", "Alice Example", "alice@example.test");
        server.add_group("platform/fabro-admins");
        server.add_group_member("platform/fabro-admins", "alice");

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = server.base_url();

        assert_eq!(
            client
                .get(format!("{base}/api/v4/user"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!("{base}/api/v4/groups"))
                .bearer_auth("wrong-token")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!(
                    "{base}/api/v4/groups/platform%2Ffabro-admins/members/all/1"
                ))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
}
