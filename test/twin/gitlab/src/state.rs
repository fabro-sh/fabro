use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::http::StatusCode;
use parking_lot::Mutex;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct GitLabUserFixture {
    pub id:       u64,
    pub username: String,
    pub name:     String,
    pub email:    String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitLabGroupFixture {
    pub id:        u64,
    pub full_path: String,
}

#[derive(Debug, Clone)]
pub struct GitLabProjectFixture {
    pub id:             u64,
    pub full_path:      String,
    pub default_branch: String,
    pub branches:       Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MergeRequestFixture {
    pub iid:           u64,
    pub title:         String,
    pub description:   String,
    pub source_branch: String,
    pub target_branch: String,
    pub web_url:       String,
    pub state:         String,
    pub merged_at:     Option<String>,
    pub created_at:    String,
    pub updated_at:    String,
    pub author:        Option<GitLabUserFixture>,
}

#[derive(Debug, Default)]
pub(crate) struct GitLabState {
    pub base_url:               String,
    pub users:                  HashMap<String, GitLabUserFixture>,
    pub groups:                 Vec<GitLabGroupFixture>,
    pub group_memberships:      HashMap<String, Vec<u64>>,
    pub projects:               HashMap<String, GitLabProjectFixture>,
    pub path_project_refs_404:  HashSet<String>,
    pub merge_requests:         HashMap<(String, u64), MergeRequestFixture>,
    pub next_merge_request_iid: u64,
    pub groups_failure:         Option<StatusCode>,
}

pub(crate) type SharedGitLabState = Arc<Mutex<GitLabState>>;
