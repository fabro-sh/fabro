use std::any::{TypeId, type_name};

use fabro_api::types::PullRequestDetail as ApiPullRequestDetail;
use fabro_types::PullRequestRecord;
use serde_json::json;

#[test]
fn pull_request_detail_reuses_domain_record_type() {
    let detail: ApiPullRequestDetail = serde_json::from_value(json!({
        "record": {
            "html_url": "https://github.com/fabro-sh/fabro/pull/123",
            "number": 123,
            "owner": "fabro-sh",
            "repo": "fabro",
            "base_branch": "main",
            "head_branch": "fabro/run/demo",
            "title": "Move PR commands server-side"
        },
        "number": 123,
        "title": "Move PR commands server-side",
        "body": "Generated body",
        "state": "closed",
        "draft": false,
        "merged": true,
        "merged_at": "2026-04-23T15:45:00Z",
        "mergeable": false,
        "additions": 234,
        "deletions": 67,
        "changed_files": 5,
        "html_url": "https://github.com/fabro-sh/fabro/pull/123",
        "user": {
            "login": "octocat"
        },
        "head": {
            "ref_name": "fabro/run/demo"
        },
        "base": {
            "ref_name": "main"
        },
        "created_at": "2026-04-23T15:40:00Z",
        "updated_at": "2026-04-23T15:45:00Z"
    }))
    .expect("detail should deserialize");

    assert_same_type_as_pull_request_record(&detail.record);
}

#[test]
fn pull_request_record_json_matches_openapi_shape() {
    let fixture = json!({
        "html_url": "https://github.com/fabro-sh/fabro/pull/123",
        "number": 123,
        "owner": "fabro-sh",
        "repo": "fabro",
        "base_branch": "main",
        "head_branch": "fabro/run/demo",
        "title": "Move PR commands server-side"
    });

    let domain_record: PullRequestRecord =
        serde_json::from_value(fixture.clone()).expect("domain record should deserialize");

    assert_eq!(serde_json::to_value(domain_record).unwrap(), fixture);
}

#[test]
fn pull_request_detail_json_matches_openapi_shape() {
    let fixture = json!({
        "record": {
            "html_url": "https://github.com/fabro-sh/fabro/pull/123",
            "number": 123,
            "owner": "fabro-sh",
            "repo": "fabro",
            "base_branch": "main",
            "head_branch": "fabro/run/demo",
            "title": "Move PR commands server-side"
        },
        "number": 123,
        "title": "Move PR commands server-side",
        "body": "Generated body",
        "state": "closed",
        "draft": false,
        "merged": true,
        "merged_at": "2026-04-23T15:45:00Z",
        "mergeable": false,
        "additions": 234,
        "deletions": 67,
        "changed_files": 5,
        "html_url": "https://github.com/fabro-sh/fabro/pull/123",
        "user": {
            "login": "octocat"
        },
        "head": {
            "ref_name": "fabro/run/demo"
        },
        "base": {
            "ref_name": "main"
        },
        "created_at": "2026-04-23T15:40:00Z",
        "updated_at": "2026-04-23T15:45:00Z"
    });

    let detail: ApiPullRequestDetail =
        serde_json::from_value(fixture.clone()).expect("detail should deserialize");

    assert_eq!(serde_json::to_value(detail).unwrap(), fixture);
}

fn assert_same_type_as_pull_request_record<T: 'static>(_: &T) {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<PullRequestRecord>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<PullRequestRecord>()
    );
}
