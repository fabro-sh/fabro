//! The run stream of a Petri run through the server: `GET /runs/{id}/events`
//! pages it by `after`, `GET /runs/{id}/attach` follows it live, and a
//! client that disconnects mid-run and reconnects from its last
//! `stream_seq` receives every item once, in order, with no gap and no
//! duplicate, including a platform record Fabro recorded between two
//! concurrent child executions' events.
//!
//! The runs execute in the server process under the handler-registry test
//! override and take their host scope through the sandbox-driver host
//! plugin, so the tests skip, and say why, when the executable is not
//! found (see `petri.rs`).
//!
//! With `FABRO_CAPTURE_PETRI_FIXTURES` set, a scenario also writes its
//! settled projection and full stream as JSON under the web app's test
//! fixtures (`apps/fabro-web/app/test-fixtures/petri/`), which the web
//! app's rendering tests read.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests locate the plugin executable and the capture switch through the process environment"
)]
#![expect(clippy::print_stderr, reason = "a skipped test says why on its stderr")]

use std::collections::BTreeSet;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fabro_server::server::AppState;
use fabro_store::platform_records::{PlatformRecord, PlatformRecordStore, RunNoticeRecord};
use fabro_types::{RunId, RunNoticeLevel, RunStreamItem, RunStreamItemKind};
use http_body_util::BodyExt;
use tokio::time::timeout;
use tower::ServiceExt;

use super::petri::{PLAIN_SETTINGS, host_plugin, intent, register_version, settled_state};
use crate::helpers::{
    api, create_and_start_run_from_intent, repo_root, response_json, run_json, settings_from_toml,
    test_app_state_with_options, test_app_with_scheduler, wait_for_run_status,
};

const CAPTURE_ENV: &str = "FABRO_CAPTURE_PETRI_FIXTURES";
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);

/// Two command branches that announce they started and wait for a release
/// marker, so a test can act between their events.
fn gated_parallel_dot(markers: &std::path::Path) -> String {
    let dir = markers.display();
    format!(
        r#"digraph Parallel {{
    graph [goal="Run two branches"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    fork [shape=component]
    a [shape=parallelogram, script="touch {dir}/a.started; while [ ! -f {dir}/go ]; do sleep 0.05; done; echo a"]
    b [shape=parallelogram, script="touch {dir}/b.started; while [ ! -f {dir}/go ]; do sleep 0.05; done; echo b"]
    merge [shape=tripleoctagon]
    start -> fork
    fork -> a
    fork -> b
    a -> merge
    b -> merge
    merge -> exit
}}"#
    )
}

/// One page of the run's stream past `after`.
async fn stream_page(
    app: &axum::Router,
    run_id: &str,
    after: u64,
    limit: usize,
) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(api(&format!(
            "/runs/{run_id}/events?after={after}&limit={limit}"
        )))
        .body(Body::empty())
        .expect("events request should build");
    let response = app
        .clone()
        .oneshot(req)
        .await
        .expect("events request routes");
    response_json(
        response,
        StatusCode::OK,
        format!("GET /api/v1/runs/{run_id}/events?after={after}&limit={limit}"),
    )
    .await
}

/// Every item of the run's stream, paged through the listing endpoint.
pub(super) async fn list_stream(
    app: &axum::Router,
    run_id: &str,
    page_limit: usize,
) -> Vec<RunStreamItem> {
    let mut after = 0;
    let mut items = Vec::new();
    loop {
        let page = stream_page(app, run_id, after, page_limit).await;
        let data: Vec<RunStreamItem> =
            serde_json::from_value(page["data"].clone()).expect("stream items decode");
        let has_more = page["meta"]["has_more"]
            .as_bool()
            .expect("has_more is a bool");
        assert_eq!(
            page["event_contract_version"].as_u64(),
            Some(u64::from(fabro_petri::petri::EVENT_CONTRACT_VERSION)),
            "the server reports Petri's contract version: {page}"
        );
        let Some(last) = data.last() else {
            assert!(!has_more, "an empty page is the last");
            break;
        };
        after = last.stream_seq;
        items.extend(data);
        if !has_more {
            break;
        }
    }
    items
}

/// An attached reader of the run's stream that stops reading when `until`
/// says so, as a client that lost its connection would: the frames it saw
/// so far come back.
struct Attached {
    body:    Body,
    pending: String,
}

impl Attached {
    async fn open(app: &axum::Router, run_id: &str, after: u64) -> Self {
        let req = Request::builder()
            .method("GET")
            .uri(api(&format!("/runs/{run_id}/attach?after={after}")))
            .body(Body::empty())
            .expect("attach request should build");
        let response = app
            .clone()
            .oneshot(req)
            .await
            .expect("attach request routes");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("text/event-stream")),
            "an SSE response"
        );
        Self {
            body:    response.into_body(),
            pending: String::new(),
        }
    }

    /// The next item on the stream, or `None` once the server ended it.
    async fn next(&mut self) -> Option<RunStreamItem> {
        loop {
            if let Some(end) = self.pending.find("\n\n") {
                let frame = self.pending[..end].to_string();
                self.pending.drain(..end + 2);
                let data = frame
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                return Some(serde_json::from_str(&data).expect("a stream item frame decodes"));
            }
            let frame = timeout(FRAME_TIMEOUT, self.body.frame())
                .await
                .expect("the attached stream keeps sending or ends");
            match frame {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        self.pending.push_str(&String::from_utf8_lossy(data));
                    }
                }
                Some(Err(err)) => panic!("the attached stream failed: {err}"),
                None => return None,
            }
        }
    }
}

fn petri_name(item: &RunStreamItem) -> Option<&str> {
    (item.kind == RunStreamItemKind::Petri)
        .then(|| item.name())
        .flatten()
}

/// The subject's node name, for a node that is a stage of its own: the
/// `parallel.branch` delegate the fork's execution holds for each branch
/// shares the branch's name and is not one.
fn subject_node(item: &RunStreamItem) -> Option<&str> {
    let node = &item.item["subject"]["node"];
    if node["meta"]["kind"].as_str() == Some("parallel.branch") {
        return None;
    }
    node["name"].as_str()
}

fn wait_for_marker(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A client attached to a two-branch parallel run disconnects once both
/// branches have started, Fabro records a platform notice while they run,
/// the client reconnects from its last `stream_seq`, and the union of what
/// it saw is the whole stream: every item once, in `stream_seq` order, no
/// gap, no duplicate, with the notice between the branches' events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnecting_client_receives_every_stream_item_once_in_order() {
    if host_plugin().is_none() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let markers = tempfile::tempdir().expect("marker tempdir");
    let settings = settings_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n");
    let state = test_app_state_with_options(settings, 5);
    let app = test_app_with_scheduler(Arc::clone(&state));

    let dot = gated_parallel_dot(markers.path());
    let version_id = register_version(&app, &[
        ("workflow.fabro", &dot),
        ("workflow.toml", PLAIN_SETTINGS),
    ])
    .await;
    let run_id =
        create_and_start_run_from_intent(&app, intent(&version_id, workspace.path())).await;
    let id: RunId = run_id.parse().expect("the run id parses");

    // First connection: from the start of the run until both branches
    // have a `visit.started` on the stream, then drop it.
    let mut first = Attached::open(&app, &run_id, 0).await;
    let mut seen_first: Vec<RunStreamItem> = Vec::new();
    let mut started: BTreeSet<String> = BTreeSet::new();
    while started.len() < 2 {
        let item = first
            .next()
            .await
            .expect("the stream runs until both branches started");
        if petri_name(&item) == Some("visit.started") {
            if let Some(node @ ("a" | "b")) = subject_node(&item) {
                started.insert(node.to_string());
            }
        }
        seen_first.push(item);
    }
    let last_seen = seen_first
        .last()
        .map(|item| item.stream_seq)
        .expect("something was seen");
    drop(first);

    // Both branch scripts are running: record a platform fact between
    // their events, as a checkpoint or a notice would be, then let them go.
    wait_for_marker(&markers.path().join("a.started"));
    wait_for_marker(&markers.path().join("b.started"));
    let platform = PlatformRecordStore::new(state.test_petri_view_pool());
    let notice = platform
        .append(
            &id,
            &PlatformRecord::RunNotice(RunNoticeRecord {
                level:   RunNoticeLevel::Info,
                code:    "test.between_branches".to_string(),
                message: "recorded while both branches ran".to_string(),
            }),
            None,
        )
        .await
        .expect("the notice appends");
    state.test_petri_projector().signal(id);
    state.test_petri_projector().settle(id).await;
    std::fs::write(markers.path().join("go"), b"").expect("the release marker writes");

    // Second connection: resume from the last stream_seq seen and read to
    // the end of the stream, which the server closes once the run is done.
    let mut second = Attached::open(&app, &run_id, last_seen).await;
    let mut seen_second: Vec<RunStreamItem> = Vec::new();
    while let Some(item) = second.next().await {
        seen_second.push(item);
    }
    let status = wait_for_run_status(&app, &run_id, &["succeeded", "failed"]).await;
    let run = run_json(&app, &run_id).await;
    assert_eq!(status, "succeeded", "run: {run}");
    let projection = settled_state(&state, &app, &run_id).await;
    assert_eq!(projection["status"]["kind"], "succeeded", "{projection}");

    // The union is the whole stream, once each, in order, with no gap.
    let mut union = seen_first;
    union.extend(seen_second);
    let seqs: Vec<u64> = union.iter().map(|item| item.stream_seq).collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(
        seqs, expected,
        "stream_seq is dense and strictly increasing"
    );
    let ids: BTreeSet<&str> = union.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids.len(), union.len(), "every item identity appears once");
    for item in &union {
        assert_eq!(item.run_id, id);
        assert!(item.recorded_at > 0, "{item:?}");
    }

    // The same stream, paged through the listing endpoint with small
    // pages, is item for item what the attached client saw.
    let listed = list_stream(&app, &run_id, 7).await;
    assert_eq!(listed, union, "the listing pages the same stream");

    // The notice sits between the branches' events.
    let notice_seq = union
        .iter()
        .find(|item| item.kind == RunStreamItemKind::Platform && item.id == notice.seq.to_string())
        .map(|item| item.stream_seq)
        .expect("the notice is on the stream");
    let branch_seqs = |name: &str| -> Vec<u64> {
        union
            .iter()
            .filter(|item| {
                petri_name(item) == Some(name) && matches!(subject_node(item), Some("a" | "b"))
            })
            .map(|item| item.stream_seq)
            .collect()
    };
    let starts = branch_seqs("visit.started");
    let ends = branch_seqs("visit.completed");
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(ends.len(), 2, "{ends:?}");
    assert!(
        starts.iter().all(|seq| *seq < notice_seq) && ends.iter().all(|seq| *seq > notice_seq),
        "the notice ({notice_seq}) is between the branch starts {starts:?} and ends {ends:?}"
    );
    let finished = union
        .iter()
        .filter(|item| petri_name(item) == Some("run.finished"))
        .count();
    assert_eq!(finished, 1, "the stream ends with the run's finish");

    capture_fixture(&app, &run_id, "parallel", &projection).await;
}

/// Write the run's settled projection and its whole stream under the web
/// app's test fixtures, when the capture switch is set.
pub(super) async fn capture_fixture(
    app: &axum::Router,
    run_id: &str,
    name: &str,
    projection: &serde_json::Value,
) {
    if env::var_os(CAPTURE_ENV).is_none() {
        return;
    }
    let stream = list_stream(app, run_id, 1000).await;
    let fixture = serde_json::json!({
        "run_id": run_id,
        "projection": projection,
        "stream": stream,
    });
    let dir = repo_root().join("apps/fabro-web/app/test-fixtures/petri");
    std::fs::create_dir_all(&dir).expect("the fixture directory creates");
    let path = dir.join(format!("{name}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&fixture).expect("the fixture serializes"),
    )
    .expect("the fixture writes");
    eprintln!("captured {}", path.display());
}

/// Helpers the other Petri scenarios use to capture their fixtures.
pub(super) async fn capture_settled(
    state: &AppState,
    app: &axum::Router,
    run_id: &str,
    name: &str,
) {
    if env::var_os(CAPTURE_ENV).is_none() {
        return;
    }
    let projection = settled_state(state, app, run_id).await;
    capture_fixture(app, run_id, name, &projection).await;
}
