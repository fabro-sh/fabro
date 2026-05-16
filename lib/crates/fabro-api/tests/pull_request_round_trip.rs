use std::any::{TypeId, type_name};

use fabro_api::types::MergeRunPullRequestRequest;
use fabro_types::settings::run::MergeStrategy;
use fabro_types::{PullRequest, PullRequestLink, PullRequestResponse};
use serde_json::json;

#[test]
fn pull_request_response_reuses_domain_types() {
    let response: PullRequestResponse = serde_json::from_value(json!({
        "data": {
            "link": {
                "owner": "fabro-sh",
                "repo": "fabro",
                "number": 123,
                "html_url": "https://github.com/fabro-sh/fabro/pull/123"
            },
            "details": null
        },
        "meta": {
            "details_status": "unavailable",
            "details_unavailable_reason": "integration_unavailable"
        }
    }))
    .expect("response should deserialize");

    assert_same_type_as_pull_request(&response.data);
    assert_same_type_as_pull_request_link(&response.data.link);
}

#[test]
fn merge_request_reuses_run_merge_strategy_type() {
    let request: MergeRunPullRequestRequest = serde_json::from_value(json!({ "method": "squash" }))
        .expect("merge request should deserialize");

    assert_same_type_as_merge_strategy(&request.method);
}

#[test]
fn merge_strategy_json_matches_openapi_shape() {
    assert_eq!(serde_json::to_value(MergeStrategy::Merge).unwrap(), "merge");
    assert_eq!(
        serde_json::to_value(MergeStrategy::Squash).unwrap(),
        "squash"
    );
    assert_eq!(
        serde_json::to_value(MergeStrategy::Rebase).unwrap(),
        "rebase"
    );
}

#[test]
fn pull_request_link_json_matches_openapi_shape() {
    let fixture = json!({
        "owner": "fabro-sh",
        "repo": "fabro",
        "number": 123,
        "html_url": "https://github.com/fabro-sh/fabro/pull/123"
    });

    let domain_record: PullRequestLink =
        serde_json::from_value(fixture.clone()).expect("domain link should deserialize");

    assert_eq!(serde_json::to_value(domain_record).unwrap(), fixture);
}

#[test]
fn pull_request_response_json_matches_openapi_shape() {
    let fixture = json!({
        "data": {
            "link": {
                "owner": "fabro-sh",
                "repo": "fabro",
                "number": 123,
                "html_url": "https://github.com/fabro-sh/fabro/pull/123"
            },
            "details": {
                "title": "Move PR commands server-side",
                "body": "Detailed description",
                "state": "closed",
                "draft": false,
                "merged": true,
                "merged_at": "2026-04-23T15:45:00Z",
                "mergeable": false,
                "additions": 234,
                "deletions": 67,
                "changed_files": 5,
                "author": {
                    "login": "octocat"
                },
                "head_branch": "fabro/run/demo",
                "base_branch": "main",
                "timestamps": {
                    "created_at": "2026-04-23T15:40:00Z",
                    "updated_at": "2026-04-23T15:45:00Z"
                }
            }
        },
        "meta": {
            "details_status": "available"
        }
    });

    let detail: PullRequestResponse =
        serde_json::from_value(fixture.clone()).expect("response should deserialize");

    assert_eq!(serde_json::to_value(detail).unwrap(), fixture);
}

fn assert_same_type_as_pull_request<T: 'static>(_: &T) {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<PullRequest>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<PullRequest>()
    );
}

fn assert_same_type_as_pull_request_link<T: 'static>(_: &T) {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<PullRequestLink>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<PullRequestLink>()
    );
}

fn assert_same_type_as_merge_strategy<T: 'static>(_: &T) {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<MergeStrategy>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<MergeStrategy>()
    );
}
