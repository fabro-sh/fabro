//! Cleanup failures leave completed workflow results intact. These tests run
//! real Host scopes and temporarily make the run's Host workspace undeletable
//! to exercise Petri's prune error path without Docker or process-wide
//! environment changes.

use fabro_petri::petri::RunKey;
use fabro_types::RunProjection;

use super::*;

struct DeletionRun {
    state:      Arc<AppState>,
    app:        Router,
    id:         RunId,
    vault_path: PathBuf,
    _source:    tempfile::TempDir,
}

fn deletion_state_builder(vault_path: PathBuf) -> TestAppStateBuilder {
    TestAppStateBuilder::new()
        .vault_path(vault_path)
        .default_environment_provider(Some(SandboxProviderKind::LOCAL))
        .runtime_settings(
            default_test_server_settings(),
            manifest_run_defaults_from_toml("_version = 1\n\n[run.environment]\nid = \"local\"\n"),
        )
}

async fn completed_deletion_run(script: &str, cancel: bool) -> DeletionRun {
    let vault_path = test_secret_store_path();
    let state = deletion_state_builder(vault_path.clone())
        .in_process_execution()
        .build();
    let app = build_test_router(Arc::clone(&state));
    spawn_scheduler(Arc::clone(&state));
    let workflow = format!(
        r#"digraph Cleanup {{
    graph [goal="Finish the required command", default_max_retries=0]
    start [shape=Mdiamond]
    work [shape=parallelogram, script="{script}", goal_gate=true, max_retries=0]
    exit [shape=Msquare]
    start -> work -> exit
}}"#,
    );
    let source = tempfile::tempdir().unwrap();
    let mut intent = test_intent(&app, &workflow).await;
    intent["target"] = json!({ "kind": "folder", "path": source.path() });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api("/runs"))
                .header("content-type", "application/json")
                .body(Body::from(intent.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = response_json!(response, StatusCode::CREATED).await;
    let id = created["id"].as_str().unwrap().parse::<RunId>().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(api(&format!("/runs/{id}/start")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::OK).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let projection = run_records::require_projection(&state, id).await.unwrap();
            if projection
                .sandbox
                .as_ref()
                .and_then(fabro_types::RunSandbox::instance)
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run acquires a Host scope");
    if cancel {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(api(&format!("/runs/{id}/cancel")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_status!(response, StatusCode::ACCEPTED).await;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let projection = run_records::require_projection(&state, id).await.unwrap();
            let key = RunKey::new(id.to_string());
            if projection.status.is_terminal()
                && state
                    .petri_runs
                    .store()
                    .owner(&key)
                    .await
                    .unwrap()
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run finishes and releases its lease");
    DeletionRun {
        state,
        app,
        id,
        vault_path,
        _source: source,
    }
}

/// Obstructs the run's Host workspace: Petri's prune removes a Host lease's
/// designated workspace itself, so a file where it expects the directory is
/// the provider failure. The workspace is kept aside, intact, for a later
/// retry; returns its path and where it was kept.
fn obstruct_host_workspace(workspace: &Path) -> (PathBuf, PathBuf) {
    let saved = workspace.with_file_name("saved-work");
    // Fault injection belongs in this server unit test: all workflow state was
    // created through the API, and only the temporary workspace is moved.
    std::fs::rename(workspace, &saved).unwrap();
    std::fs::write(workspace, "provider unavailable").unwrap();
    (workspace.to_path_buf(), saved)
}

async fn host_workspace(run: &DeletionRun) -> PathBuf {
    let projection = run_records::require_projection(&run.state, run.id)
        .await
        .unwrap();
    PathBuf::from(
        &projection
            .sandbox
            .as_ref()
            .unwrap()
            .instance()
            .unwrap()
            .runtime
            .working_directory,
    )
}

async fn deletion_response(app: &Router, id: RunId, force: bool) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(api(&format!("/runs/{id}?force={force}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn assert_completed_result(app: &Router, id: RunId, before: &RunProjection) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(api(&format!("/runs/{id}/state")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let after = response_json!(response, StatusCode::OK).await;
    assert_eq!(
        after["status"],
        serde_json::to_value(before.status).unwrap()
    );
    assert_eq!(
        after["status_updated_at"],
        serde_json::to_value(before.status_updated_at).unwrap()
    );
    assert_eq!(
        after["conclusion"],
        serde_json::to_value(&before.conclusion).unwrap()
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(api("/runs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = response_json!(response, StatusCode::OK).await;
    assert!(
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run_json_id(run) == Some(id.to_string().as_str())),
        "failed cleanup must not hide the run from the default list"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cleanup_preserves_completed_results_across_retries_and_restart() {
    for (script, cancel, expected) in [
        ("echo completed", false, RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        }),
        ("exit 1", false, RunStatus::Failed {
            reason: FailureReason::WorkflowError,
        }),
        ("sleep 30", true, RunStatus::Failed {
            reason: FailureReason::Cancelled,
        }),
    ] {
        let run = completed_deletion_run(script, cancel).await;
        let before = run_records::require_projection(&run.state, run.id)
            .await
            .unwrap();
        assert_eq!(before.status, expected);
        let workspace = host_workspace(&run).await;
        let (obstructed, saved) = obstruct_host_workspace(&workspace);
        for _ in 0..2 {
            let response = deletion_response(&run.app, run.id, false).await;
            let error = response_json!(response, StatusCode::CONFLICT).await;
            let detail = error["errors"][0]["detail"].as_str().unwrap();
            assert!(detail.contains("Failed to delete the run's sandboxes"));
            assert!(detail.contains("use force=true or --force"));
            assert!(saved.is_dir());
            assert_completed_result(&run.app, run.id, &before).await;
        }
        run.state.shutdown.cancel();
        let after = deletion_state_builder(run.vault_path.clone())
            .store_bundle(
                Arc::clone(&run.state.stores.runs),
                run.state.artifact_store.clone(),
            )
            .build();
        let app = build_test_router(Arc::clone(&after));
        assert_eq!(
            reconcile_incomplete_runs_on_startup(&after).await.unwrap(),
            0
        );
        assert_completed_result(&app, run.id, &before).await;
        let response = deletion_response(&app, run.id, false).await;
        assert_status!(response, StatusCode::CONFLICT).await;
        assert_completed_result(&app, run.id, &before).await;
        std::fs::remove_file(&obstructed).unwrap();
        std::fs::rename(saved, &obstructed).unwrap();
        let response = deletion_response(&app, run.id, false).await;
        assert_status!(response, StatusCode::NO_CONTENT).await;
        assert!(!workspace.exists());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(api(&format!("/runs/{}", run.id)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_status!(response, StatusCode::NOT_FOUND).await;
        after.shutdown.cancel();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_explicit_force_bypasses_cleanup_failure_for_a_removing_run() {
    let run = completed_deletion_run("echo completed", false).await;
    obstruct_host_workspace(&host_workspace(&run).await);
    // Exercise the existing Removing path independently: it must not make an
    // ordinary delete bypass a provider failure. No historical repair is done.
    run_records::lifecycle(
        &run.state,
        run.id,
        run_records::transition(RunLifecycleKind::Removing, RunStatus::Removing),
    )
    .await
    .unwrap();
    let response = deletion_response(&run.app, run.id, false).await;
    assert_status!(response, StatusCode::CONFLICT).await;
    let response = deletion_response(&run.app, run.id, true).await;
    assert_status!(response, StatusCode::NO_CONTENT).await;
    let response = run
        .app
        .oneshot(
            Request::builder()
                .uri(api(&format!("/runs/{}", run.id)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
    run.state.shutdown.cancel();
}
