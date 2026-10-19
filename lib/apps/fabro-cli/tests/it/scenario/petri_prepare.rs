//! Server settings must reach admission and execute in the worker, before
//! user commands. These controls exercise the same path with workflow settings.

#![expect(
    clippy::disallowed_methods,
    reason = "integration fixtures use synchronous filesystem operations"
)]

use fabro_test::test_context;
use fabro_util::shell;
use serde_json::json;

use super::petri::{self, RunningServer};

async fn prepare_run(server_owned: bool, failing: bool) {
    let context = test_context!();
    let script = if failing {
        "exit 23"
    } else {
        "printf prepared > prepare-marker"
    };
    let prepare = format!("\n[[run.prepare.steps]]\nscript = {script:?}\n");
    let server = RunningServer::start_with(if server_owned { &prepare } else { "" }, &[]).await;
    let user = context.temp_dir.join("user-marker");
    let started = context.temp_dir.join("user-started");
    let command = format!(
        "printf started > {}; cat prepare-marker > {}",
        shell::shell_quote(&started.to_string_lossy()),
        shell::shell_quote(&user.to_string_lossy()),
    );
    let command = serde_json::to_string(&command).expect("the command encodes as a DOT string");
    let workspace = petri::write_petri_workflow(
        &context,
        &format!(
            r"digraph Prepare {{
        graph [default_max_retries=0]
        start [shape=Mdiamond]
        work [shape=parallelogram, script={command}, max_retries=0]
        exit [shape=Msquare]
        start -> work -> exit
    }}"
        ),
    );
    if !server_owned {
        let path = workspace.join("workflow.toml");
        let config = std::fs::read_to_string(&path).expect("the workflow settings read");
        std::fs::write(path, format!("{config}{prepare}")).expect("the prepare settings write");
    }
    let run_id = petri::run_detached(&context, &server, &workspace);
    let status = petri::wait_for_status(&server, &run_id, &["succeeded", "failed"]).await;
    let state = petri::run_json(&server, &format!("runs/{run_id}/state")).await;
    assert_eq!(
        state["spec"]["settings"]["run"]["prepare"]["steps"],
        json!([{ "type": "script", "script": script }])
    );
    if failing {
        assert_eq!(status, "failed", "{state}");
        assert!(
            !started.exists(),
            "user work must not start after failed preparation"
        );
        assert!(!user.exists());
    } else {
        assert_eq!(status, "succeeded", "{state}");
        assert_eq!(
            std::fs::read_to_string(user).expect("the user command wrote its marker"),
            "prepared"
        );
    }
    assert!(
        state["spec"]["graph"]["nodes"]
            .get("run_prepare_1")
            .is_some(),
        "the public run graph must include preparation: {state}"
    );
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn server_prepare_runs_before_user_command() {
    prepare_run(true, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_prepare_failure_prevents_user_command() {
    prepare_run(true, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_prepare_runs_before_user_command() {
    prepare_run(false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_prepare_failure_prevents_user_command() {
    prepare_run(false, true).await;
}
