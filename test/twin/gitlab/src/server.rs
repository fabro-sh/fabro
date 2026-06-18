use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use parking_lot::Mutex;
use tokio::net::TcpListener;

use crate::state::{
    GitLabGroupFixture, GitLabProjectFixture, GitLabState, GitLabUserFixture, SharedGitLabState,
};
use crate::{api, oauth};

#[derive(Debug)]
pub struct TestGitLabServer {
    base_url:         String,
    state:            SharedGitLabState,
    automation_token: String,
}

impl TestGitLabServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test GitLab server should bind an ephemeral port");
        let addr: SocketAddr = listener
            .local_addr()
            .expect("bound test GitLab server should have a local address");
        let base_url = format!("http://{addr}");
        let state = Arc::new(Mutex::new(GitLabState {
            base_url: base_url.clone(),
            next_merge_request_iid: 1,
            ..GitLabState::default()
        }));
        let automation_token = "gitlab-automation-token".to_string();
        let app = Router::new()
            .route("/oauth/authorize", get(oauth::authorize))
            .route("/oauth/token", post(oauth::token))
            .route("/api/v4/user", get(api::user))
            .route("/api/v4/groups", get(api::groups))
            .route(
                "/api/v4/groups/{group_path}/members/all/{user_id}",
                get(api::group_member),
            )
            .route("/api/v4/projects", get(api::projects))
            .route("/api/v4/projects/{*path}", get(api::project_get))
            .route("/api/v4/projects/{*path}", post(api::project_post))
            .route("/api/v4/projects/{*path}", put(api::project_put))
            .with_state((state.clone(), automation_token.clone()));

        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test GitLab server should serve requests");
        });

        Self {
            base_url,
            state,
            automation_token,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn automation_token(&self) -> &str {
        &self.automation_token
    }

    pub fn add_user(&self, username: &str, name: &str, email: &str) {
        let mut state = self.state.lock();
        let id = state.users.len() as u64 + 1;
        state.users.insert(username.to_string(), GitLabUserFixture {
            id,
            username: username.to_string(),
            name: name.to_string(),
            email: email.to_string(),
        });
    }

    pub fn add_group(&self, full_path: &str) {
        let mut state = self.state.lock();
        let id = state.groups.len() as u64 + 1;
        state.groups.push(GitLabGroupFixture {
            id,
            full_path: full_path.to_string(),
        });
    }

    pub fn add_group_member(&self, full_path: &str, username: &str) {
        let mut state = self.state.lock();
        let user_id = state
            .users
            .get(username)
            .unwrap_or_else(|| panic!("GitLab test user {username} should exist"))
            .id;
        state
            .group_memberships
            .entry(full_path.to_string())
            .or_default()
            .push(user_id);
    }

    pub fn fail_groups_with(&self, status: StatusCode) {
        self.state.lock().groups_failure = Some(status);
    }

    pub fn add_project(&self, full_path: &str, default_branch: &str, branches: &[&str]) {
        let id = {
            let state = self.state.lock();
            state.projects.len() as u64 + 1
        };
        self.add_project_with_id(id, full_path, default_branch, branches);
    }

    pub fn add_project_with_id(
        &self,
        id: u64,
        full_path: &str,
        default_branch: &str,
        branches: &[&str],
    ) {
        let mut all_branches = vec![default_branch.to_string()];
        for branch in branches {
            let branch = (*branch).to_string();
            if !all_branches.contains(&branch) {
                all_branches.push(branch);
            }
        }

        self.state
            .lock()
            .projects
            .insert(full_path.to_string(), GitLabProjectFixture {
                id,
                full_path: full_path.to_string(),
                default_branch: default_branch.to_string(),
                branches: all_branches,
            });
    }

    pub fn fail_project_path_refs_for(&self, full_path: &str) {
        self.state
            .lock()
            .path_project_refs_404
            .insert(full_path.to_string());
    }
}
