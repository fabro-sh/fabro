use axum::Json;
use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::state::{GitLabUserFixture, MergeRequestFixture, SharedGitLabState};

type AppState = (SharedGitLabState, String);

const NOW: &str = "2026-06-13T00:00:00Z";

pub(crate) async fn user(State((state, _)): State<AppState>) -> Response {
    let state = state.lock();
    let user = state
        .users
        .values()
        .next()
        .cloned()
        .unwrap_or_else(default_user);
    Json(user).into_response()
}

pub(crate) async fn groups(State((state, _)): State<AppState>) -> Response {
    let state = state.lock();
    if let Some(status) = state.groups_failure {
        return (status, Json(json!({ "message": "injected group failure" }))).into_response();
    }

    Json(state.groups.clone()).into_response()
}

pub(crate) async fn group_member(
    State((state, _)): State<AppState>,
    Path((group_path, user_id)): Path<(String, u64)>,
) -> Response {
    let state = state.lock();
    if let Some(status) = state.groups_failure {
        return (status, Json(json!({ "message": "injected group failure" }))).into_response();
    }

    let group_path = percent_decode_str(&group_path).decode_utf8_lossy();
    let Some(group) = state
        .groups
        .iter()
        .find(|group| group.full_path == group_path)
    else {
        return not_found();
    };
    if state
        .group_memberships
        .get(group.full_path.as_str())
        .is_some_and(|members| members.contains(&user_id))
    {
        Json(json!({
            "id": user_id,
            "username": state
                .users
                .values()
                .find(|user| user.id == user_id)
                .map(|user| user.username.clone())
                .unwrap_or_else(|| format!("user-{user_id}")),
        }))
        .into_response()
    } else {
        not_found()
    }
}

pub(crate) async fn project_get(
    State(app_state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some((state, _automation_token)) = authorize_project_request(&app_state, &headers) else {
        return unauthorized();
    };

    if let Some((project_id, branch_name)) = branch_request_parts(&path) {
        return branch(&state, &project_id, &branch_name);
    }

    if let Some((project_id, iid)) = merge_request_request_parts(&path, false) {
        return get_merge_request(&state, &project_id, iid);
    }

    project(&state, &decode_project_component(&path))
}

pub(crate) async fn project_post(
    State(app_state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    Json(input): Json<CreateMergeRequestInput>,
) -> Response {
    let Some((state, _automation_token)) = authorize_project_request(&app_state, &headers) else {
        return unauthorized();
    };

    let Some(project_id) = path
        .strip_suffix("/merge_requests")
        .map(decode_project_component)
    else {
        return not_found();
    };

    create_merge_request(&state, &project_id, input)
}

pub(crate) async fn project_put(
    State(app_state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Option<Json<UpdateMergeRequestInput>>,
) -> Response {
    let Some((state, _automation_token)) = authorize_project_request(&app_state, &headers) else {
        return unauthorized();
    };

    if let Some((project_id, iid)) = merge_request_request_parts(&path, true) {
        return merge_merge_request(&state, &project_id, iid);
    }

    let Some((project_id, iid)) = merge_request_request_parts(&path, false) else {
        return not_found();
    };

    let state_event = body.and_then(|Json(input)| input.state_event);
    if state_event.as_deref() == Some("close") {
        return close_merge_request(&state, &project_id, iid);
    }

    get_merge_request(&state, &project_id, iid)
}

fn authorize_project_request(
    (state, automation_token): &AppState,
    headers: &HeaderMap,
) -> Option<(SharedGitLabState, String)> {
    let expected = format!("Bearer {automation_token}");
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|actual| actual == expected);
    let private_token = headers
        .get("private-token")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|actual| actual == automation_token);
    (bearer || private_token).then(|| (state.clone(), automation_token.clone()))
}

fn branch_request_parts(path: &str) -> Option<(String, String)> {
    let (project_id, branch) = path.split_once("/repository/branches/")?;
    Some((
        decode_project_component(project_id),
        decode_project_component(branch),
    ))
}

fn merge_request_request_parts(path: &str, merge: bool) -> Option<(String, u64)> {
    let path = if merge {
        path.strip_suffix("/merge")?
    } else {
        path
    };
    let (project_id, iid) = path.split_once("/merge_requests/")?;
    if iid.contains('/') {
        return None;
    }
    Some((decode_project_component(project_id), iid.parse().ok()?))
}

fn decode_project_component(value: &str) -> String {
    percent_decode_str(value).decode_utf8_lossy().into_owned()
}

fn branch(state: &SharedGitLabState, project_id: &str, branch: &str) -> Response {
    let state = state.lock();
    let Some(project) = state.projects.get(project_id) else {
        return not_found();
    };
    if !project.branches.iter().any(|candidate| candidate == branch) {
        return not_found();
    }

    Json(json!({
        "name": branch,
        "default": branch == project.default_branch,
        "web_url": format!("{}/{}/-/tree/{}", state.base_url, project.full_path, branch),
    }))
    .into_response()
}

fn project(state: &SharedGitLabState, project_id: &str) -> Response {
    let state = state.lock();
    let Some(project) = state.projects.get(project_id) else {
        return not_found();
    };

    Json(json!({
        "id": project_id,
        "path_with_namespace": project.full_path,
        "default_branch": project.default_branch,
        "web_url": format!("{}/{}", state.base_url, project.full_path),
    }))
    .into_response()
}

fn create_merge_request(
    state: &SharedGitLabState,
    project_id: &str,
    input: CreateMergeRequestInput,
) -> Response {
    let mut state = state.lock();
    if !state.projects.contains_key(project_id) {
        return not_found();
    }

    let iid = state.next_merge_request_iid;
    state.next_merge_request_iid += 1;
    let web_url = format!("{}/{project_id}/-/merge_requests/{iid}", state.base_url);
    let author = state
        .users
        .values()
        .next()
        .cloned()
        .unwrap_or_else(default_user);
    let merge_request = MergeRequestFixture {
        iid,
        title: input.title,
        description: input.description.unwrap_or_default(),
        source_branch: input.source_branch,
        target_branch: input.target_branch,
        web_url,
        state: "opened".to_string(),
        merged_at: None,
        created_at: NOW.to_string(),
        updated_at: NOW.to_string(),
        author: Some(author),
    };
    let body = merge_request_json(project_id, &merge_request);
    state
        .merge_requests
        .insert((project_id.to_string(), iid), merge_request);

    (StatusCode::CREATED, Json(body)).into_response()
}

fn get_merge_request(state: &SharedGitLabState, project_id: &str, iid: u64) -> Response {
    let state = state.lock();
    let Some(merge_request) = state.merge_requests.get(&(project_id.to_string(), iid)) else {
        return not_found();
    };

    Json(merge_request_json(project_id, merge_request)).into_response()
}

fn merge_merge_request(state: &SharedGitLabState, project_id: &str, iid: u64) -> Response {
    let mut state = state.lock();
    let Some(merge_request) = state.merge_requests.get_mut(&(project_id.to_string(), iid)) else {
        return not_found();
    };

    merge_request.state = "merged".to_string();
    merge_request.merged_at = Some(NOW.to_string());
    merge_request.updated_at = NOW.to_string();
    Json(merge_request_json(project_id, merge_request)).into_response()
}

fn close_merge_request(state: &SharedGitLabState, project_id: &str, iid: u64) -> Response {
    let mut state = state.lock();
    let Some(merge_request) = state.merge_requests.get_mut(&(project_id.to_string(), iid)) else {
        return not_found();
    };

    merge_request.state = "closed".to_string();
    merge_request.updated_at = NOW.to_string();
    Json(merge_request_json(project_id, merge_request)).into_response()
}

fn merge_request_json(_project_id: &str, merge_request: &MergeRequestFixture) -> Value {
    json!({
        "iid": merge_request.iid,
        "title": merge_request.title,
        "description": merge_request.description,
        "web_url": merge_request.web_url,
        "state": merge_request.state,
        "draft": false,
        "work_in_progress": false,
        "merged_at": merge_request.merged_at,
        "merge_status": "can_be_merged",
        "detailed_merge_status": "mergeable",
        "changes_count": "1",
        "source_branch": merge_request.source_branch,
        "target_branch": merge_request.target_branch,
        "author": merge_request.author.clone().unwrap_or_else(default_user),
        "created_at": merge_request.created_at,
        "updated_at": merge_request.updated_at,
    })
}

fn default_user() -> GitLabUserFixture {
    GitLabUserFixture {
        id:       1,
        username: "automation".to_string(),
        name:     "Automation".to_string(),
        email:    "automation@example.test".to_string(),
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "message": "401 Unauthorized" })),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": "404 Not Found" })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateMergeRequestInput {
    source_branch:         String,
    target_branch:         String,
    title:                 String,
    description:           Option<String>,
    #[serde(rename = "remove_source_branch")]
    _remove_source_branch: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateMergeRequestInput {
    state_event: Option<String>,
}
