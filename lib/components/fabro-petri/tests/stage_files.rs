//! A tracked native edit reaches both Fabro's public stage view and the next
//! prompt.

mod support;

use std::sync::Arc;

use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::projection::{Item, RunView};
use fabro_petri::runtime::RuntimeSpec;
use fabro_store::platform_records::{PlatformRecord, RunCreatedRecord, StoredPlatformRecord};
use fabro_types::{RunId, StageId, test_support as types_support};
use pebble_coding_agent::test_support::{
    ScriptedCall, message_text, scripted_client, text_response, tool_call_response,
};
use petri_execution::events::replay_run;
use petri_store::{Access, MemoryRunStore, RunKey, RunStore};
use serde_json::json;
use support::{Silent, no_questions, run_request};

#[tokio::test]
async fn compact_successor_receives_tracked_native_files() {
    let root = tempfile::tempdir().expect("runtime directory");
    let (client, provider) = scripted_client(vec![
        ScriptedCall::response(tool_call_response(
            "write_file",
            "write",
            json!({"file_path":"direct-edit.txt", "content":"changed"}),
        )),
        ScriptedCall::response(text_response("Done.")),
        ScriptedCall::response(text_response("Reviewed.")),
    ]);
    let runtime = RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    };
    let workflow = r#"digraph Files {
        graph [backend="api", default_max_retries=0]
        start [shape=Mdiamond]
        edit [prompt="Write the file"]
        review [prompt="Review the edit", fidelity="compact"]
        exit [shape=Msquare]
        start -> edit -> review -> exit
    }"#;
    let graphs = support::admit(
        &[
            ("workflow.fabro", workflow),
            ("workflow.toml", support::SETTINGS),
        ],
        Launch {
            default_model: Some("test/model".into()),
            ..Launch::default()
        },
        &runtime,
    );
    let store = Arc::new(MemoryRunStore::new());
    let run_id = RunId::new();
    let outcome = engine::run(run_request(
        &run_id.to_string(),
        &root.path().join("run"),
        graphs,
        store.clone(),
        runtime,
        no_questions(Arc::new(Silent)),
    ))
    .await
    .expect("workflow run settles");
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let logs = store
        .open(&RunKey::new(run_id.to_string()), Access::Read)
        .await
        .expect("stored run");
    let events = replay_run(&*logs).await.expect("replayed run");
    let mut spec = types_support::test_run_spec();
    spec.run_id = run_id;
    let created = StoredPlatformRecord {
        seq:         1,
        recorded_at: 0,
        position:    None,
        record:      PlatformRecord::RunCreated(RunCreatedRecord {
            spec,
            title: Some("Tracked files".into()),
            parent_id: None,
            retried_from: None,
            web_url: None,
        }),
    };
    let mut view = RunView::new();
    view.fold(&Item::Platform(&created), 1);
    for (index, event) in events.iter().enumerate() {
        view.fold(&Item::Petri(event), index as u64 + 2);
    }
    let projection = view.projection().expect("public run projection");
    let edit = projection
        .stage(&StageId::new("edit", 1))
        .expect("edit stage");
    assert_eq!(
        edit.agent
            .as_ref()
            .expect("native agent")
            .totals
            .files_touched,
        ["direct-edit.txt"]
    );
    let review = projection
        .stage(&StageId::new("review", 1))
        .expect("review stage");
    assert!(
        review
            .agent
            .as_ref()
            .expect("native agent")
            .totals
            .files_touched
            .is_empty()
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let sent = message_text(requests[2].messages().last().expect("review input"));
    assert!(sent.contains("- **edit**: succeeded"), "{sent}");
    assert!(sent.contains("Files: [\"direct-edit.txt\"]"), "{sent}");
}
