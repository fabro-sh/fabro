use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use serde::Deserialize;

use crate::state::{
    AppOptions, AppState, Comment, Installation, IssueContent, ManifestConversion, OwnerType,
    Project, ProjectItem, PullRequest, Release, Repository, StatusOption, TokenInfo,
    WebhookOptions,
};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FixtureState {
    #[serde(default)]
    pub apps: Vec<FixtureApp>,
    #[serde(default)]
    pub installations: Vec<FixtureInstallation>,
    #[serde(default)]
    pub repositories: Vec<FixtureRepository>,
    #[serde(default)]
    pub pull_requests: Vec<FixturePullRequest>,
    #[serde(default)]
    pub active_tokens: Vec<FixtureActiveToken>,
    #[serde(default)]
    pub projects: Vec<FixtureProject>,
    #[serde(default)]
    pub releases: Vec<FixtureRelease>,
    #[serde(default)]
    pub manifest_conversions: Vec<FixtureManifestConversion>,
    #[serde(default)]
    pub comments: Vec<FixtureComment>,
    #[serde(default)]
    pub webhook_config: FixtureWebhookOptions,
    pub next_installation_id: Option<u64>,
    pub next_pr_number: Option<u64>,
    pub viewer_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureApp {
    pub app_id: String,
    pub slug: String,
    pub owner_login: String,
    pub public: bool,
    pub private_key_pem: String,
    pub webhook_secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureInstallation {
    pub id: u64,
    pub app_id: String,
    pub owner: String,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub suspended: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRepository {
    pub owner: String,
    pub name: String,
    #[serde(default)]
    pub branches: Vec<String>,
    pub default_branch: Option<String>,
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixturePullRequest {
    pub owner: String,
    pub repo: String,
    #[serde(flatten)]
    pub pull_request: PullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureActiveToken {
    pub token: String,
    pub app_id: String,
    pub installation_id: u64,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default = "empty_json_object")]
    pub permissions: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureProject {
    pub node_id: String,
    pub number: u64,
    pub owner: String,
    pub owner_type: FixtureOwnerType,
    pub status_field_id: String,
    #[serde(default)]
    pub status_options: Vec<FixtureStatusOption>,
    #[serde(default)]
    pub items: Vec<FixtureProjectItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixtureOwnerType {
    Organization,
    User,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureStatusOption {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureProjectItem {
    pub id: String,
    pub status: String,
    pub content: FixtureIssueContent,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureIssueContent {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub assignee_ids: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRelease {
    pub owner: String,
    pub repo: String,
    pub tag_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureManifestConversion {
    pub code: String,
    pub app_id: i64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub webhook_secret: Option<String>,
    pub pem: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureComment {
    pub issue_node_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FixtureWebhookOptions {
    pub url: Option<String>,
    pub content_type: Option<String>,
}

impl FixtureState {
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)
            .map_err(|err| format!("failed to read fixture {}: {err}", path.display()))?;
        serde_json::from_str(&contents)
            .map_err(|err| format!("failed to parse fixture {}: {err}", path.display()))
    }

    pub fn into_app_state(self) -> Result<AppState, String> {
        let mut state = AppState::new();

        for app in self.apps {
            let app_id = app.app_id.clone();
            let config = AppOptions {
                app_id: app.app_id,
                slug: app.slug,
                owner_login: app.owner_login,
                public: app.public,
                private_key_pem: app.private_key_pem,
                webhook_secret: app.webhook_secret,
            };

            catch_unwind(AssertUnwindSafe(|| state.register_app(config)))
                .map_err(|_| format!("invalid RSA private key PEM for fixture app {app_id}"))?;
        }

        state.installations = self
            .installations
            .into_iter()
            .map(|installation| Installation {
                id: installation.id,
                app_id: installation.app_id,
                owner: installation.owner,
                repositories: installation.repositories,
                suspended: installation.suspended,
            })
            .collect();

        state.repositories = self
            .repositories
            .into_iter()
            .map(|repository| Repository {
                owner: repository.owner,
                name: repository.name,
                branches: repository.branches,
                default_branch: repository
                    .default_branch
                    .unwrap_or_else(|| "main".to_string()),
                private: repository.private,
                git_dir: None,
            })
            .collect();

        for pull_request in self.pull_requests {
            state
                .pull_requests
                .entry((pull_request.owner, pull_request.repo))
                .or_default()
                .push(pull_request.pull_request);
        }

        state.active_tokens = self
            .active_tokens
            .into_iter()
            .map(|token| {
                (
                    token.token,
                    TokenInfo {
                        app_id: token.app_id,
                        installation_id: token.installation_id,
                        repositories: token.repositories,
                        permissions: token.permissions,
                    },
                )
            })
            .collect();

        state.projects = self
            .projects
            .into_iter()
            .map(|project| Project {
                node_id: project.node_id,
                number: project.number,
                owner: project.owner,
                owner_type: match project.owner_type {
                    FixtureOwnerType::Organization => OwnerType::Organization,
                    FixtureOwnerType::User => OwnerType::User,
                },
                status_field_id: project.status_field_id,
                status_options: project
                    .status_options
                    .into_iter()
                    .map(|option| StatusOption {
                        id: option.id,
                        name: option.name,
                    })
                    .collect(),
                items: project
                    .items
                    .into_iter()
                    .map(|item| ProjectItem {
                        id: item.id,
                        status: item.status,
                        content: IssueContent {
                            id: item.content.id,
                            number: item.content.number,
                            title: item.content.title,
                            body: item.content.body,
                            url: item.content.url,
                            created_at: item.content.created_at,
                            updated_at: item.content.updated_at,
                            assignee_ids: item.content.assignee_ids,
                            labels: item.content.labels,
                        },
                    })
                    .collect(),
            })
            .collect();

        state.releases = self
            .releases
            .into_iter()
            .map(|release| {
                (
                    (release.owner, release.repo),
                    Release {
                        tag_name: release.tag_name,
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        state.manifest_conversions = self
            .manifest_conversions
            .into_iter()
            .map(|conversion| {
                let code = conversion.code.clone();
                (
                    code,
                    ManifestConversion {
                        code: conversion.code,
                        app_id: conversion.app_id,
                        slug: conversion.slug,
                        client_id: conversion.client_id,
                        client_secret: conversion.client_secret,
                        webhook_secret: conversion.webhook_secret,
                        pem: conversion.pem,
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        state.comments = self
            .comments
            .into_iter()
            .map(|comment| Comment {
                issue_node_id: comment.issue_node_id,
                body: comment.body,
            })
            .collect();

        state.webhook_config = WebhookOptions {
            url: self.webhook_config.url,
            content_type: self.webhook_config.content_type,
        };

        state.next_installation_id = self
            .next_installation_id
            .unwrap_or_else(|| next_installation_id(&state.installations));
        state.next_pr_number = self
            .next_pr_number
            .unwrap_or_else(|| next_pr_number(&state.pull_requests));

        if let Some(viewer_id) = self.viewer_id {
            state.viewer_id = viewer_id;
        }

        Ok(state)
    }
}

fn empty_json_object() -> serde_json::Value {
    serde_json::json!({})
}

fn next_installation_id(installations: &[Installation]) -> u64 {
    installations
        .iter()
        .map(|installation| installation.id)
        .max()
        .unwrap_or(0)
        + 1
}

fn next_pr_number(pull_requests: &HashMap<(String, String), Vec<PullRequest>>) -> u64 {
    pull_requests
        .values()
        .flat_map(|prs| prs.iter().map(|pr| pr.number))
        .max()
        .unwrap_or(0)
        + 1
}

#[cfg(test)]
impl FixtureState {
    fn single_app_fixture_for_test() -> Self {
        Self {
            apps: vec![FixtureApp {
                app_id: "100".to_string(),
                slug: "fixture-app".to_string(),
                owner_login: "acme".to_string(),
                public: true,
                private_key_pem: test_rsa_key().to_string(),
                webhook_secret: Some("whsec".to_string()),
            }],
            ..Self::default()
        }
    }
}

#[cfg(test)]
fn test_rsa_key() -> &'static str {
    crate::test_support::test_rsa_private_key()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_state_loads_full_server_state() {
        let private_key_pem = test_rsa_key().replace('\n', "\\n");
        let fixture_json = r#"{
            "apps": [{
                "app_id": "100",
                "slug": "ops-app",
                "owner_login": "acme",
                "public": true,
                "private_key_pem": "__PRIVATE_KEY__",
                "webhook_secret": "whsec"
            }],
            "installations": [{
                "id": 7,
                "app_id": "100",
                "owner": "acme",
                "repositories": ["widgets"],
                "suspended": false
            }],
            "repositories": [{
                "owner": "acme",
                "name": "widgets",
                "branches": ["main", "feature"],
                "default_branch": "main"
            }],
            "pull_requests": [{
                "owner": "acme",
                "repo": "widgets",
                "number": 1,
                "node_id": "PR_1",
                "title": "Ship it",
                "body": "Ready",
                "state": "open",
                "draft": false,
                "mergeable": true,
                "additions": 10,
                "deletions": 5,
                "changed_files": 2,
                "html_url": "https://github.com/acme/widgets/pull/1",
                "user_login": "bot",
                "head_ref": "feature",
                "base_ref": "main",
                "created_at": "2026-03-27T00:00:00Z",
                "updated_at": "2026-03-27T00:00:00Z",
                "auto_merge": null
            }],
            "active_tokens": [{
                "token": "ghs_exampletoken",
                "app_id": "100",
                "installation_id": 7,
                "repositories": ["widgets"],
                "permissions": {"contents": "write"}
            }],
            "projects": [{
                "node_id": "PVT_kwDOB",
                "number": 3,
                "owner": "acme",
                "owner_type": "organization",
                "status_field_id": "PVTSSF_1",
                "status_options": [{
                    "id": "opt_todo",
                    "name": "Todo"
                }],
                "items": [{
                    "id": "PVTI_1",
                    "status": "Todo",
                    "content": {
                        "id": "ISSUE_1",
                        "number": 42,
                        "title": "Track launch",
                        "body": "Ship it",
                        "url": "https://github.com/acme/widgets/issues/42",
                        "created_at": "2026-03-27T00:00:00Z",
                        "updated_at": "2026-03-27T00:00:00Z",
                        "assignee_ids": ["U_1"],
                        "labels": ["priority"]
                    }
                }]
            }],
            "releases": [{
                "owner": "acme",
                "repo": "widgets",
                "tag_name": "v1.2.3"
            }],
            "manifest_conversions": [{
                "code": "manifest-code",
                "app_id": 100,
                "slug": "ops-app-dev",
                "client_id": "Iv1.123",
                "client_secret": "manifest-secret",
                "webhook_secret": "manifest-whsec",
                "pem": "manifest pem"
            }],
            "comments": [{
                "issue_node_id": "ISSUE_1",
                "body": "Looks good"
            }],
            "webhook_config": {
                "url": "https://example.com/webhooks/github",
                "content_type": "json"
            },
            "next_installation_id": 99,
            "next_pr_number": 77,
            "viewer_id": "U_seeded"
        }"#
        .replace("__PRIVATE_KEY__", &private_key_pem);
        let fixture: FixtureState = serde_json::from_str(&fixture_json).unwrap();

        let state = fixture.into_app_state().unwrap();

        assert_eq!(state.apps["100"].config.slug, "ops-app");
        assert_eq!(state.installations.len(), 1);
        assert_eq!(state.repositories.len(), 1);
        assert_eq!(
            state.pull_requests[&("acme".into(), "widgets".into())].len(),
            1
        );
        assert_eq!(state.active_tokens.len(), 1);
        assert_eq!(state.projects.len(), 1);
        assert_eq!(
            state.releases[&("acme".into(), "widgets".into())].tag_name,
            "v1.2.3"
        );
        assert_eq!(
            state.manifest_conversions["manifest-code"].client_secret,
            "manifest-secret"
        );
        assert_eq!(state.comments.len(), 1);
        assert_eq!(
            state.webhook_config.url.as_deref(),
            Some("https://example.com/webhooks/github")
        );
        assert_eq!(state.webhook_config.content_type.as_deref(), Some("json"));
        assert_eq!(state.next_installation_id, 99);
        assert_eq!(state.next_pr_number, 77);
        assert_eq!(state.viewer_id, "U_seeded");
    }

    #[test]
    fn fixture_state_derives_public_keys_for_registered_apps() {
        let fixture = FixtureState::single_app_fixture_for_test();
        let state = fixture.into_app_state().unwrap();
        assert!(state.apps["100"].public_key_pem.contains("PUBLIC KEY"));
    }
}
