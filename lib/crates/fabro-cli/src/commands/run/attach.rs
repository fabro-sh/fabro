use std::io::{IsTerminal, Write};
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use fabro_api::types;
use fabro_interview::{AnswerValue, ConsoleInterviewer, Question, QuestionOption, QuestionType};
use fabro_store::EventEnvelope;
use fabro_types::settings::cli::OutputVerbosity;
use fabro_types::settings::run::ApprovalMode;
use fabro_types::{EventBody, RunEvent, RunId};
use fabro_util::json::normalize_json_value;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::run_status::RunStatus;
use tokio::signal::ctrl_c;
use tokio::time::sleep;

use super::run_progress;
use crate::server_client;

const INTERVIEW_UNANSWERED_MESSAGE: &str =
    "Interview ended without an answer. The run is still waiting for input; reattach to answer it.";
const JSON_INTERVIEW_MESSAGE: &str = "This run is waiting for human input, but --json is non-interactive. Reattach without --json to answer it.";
const ATTACH_PREMATURE_EOF_MESSAGE: &str = "Attach stream ended before terminal run event.";

/// Attach to a running (or finished) workflow run, rendering progress live.
///
/// Returns exit code 0 for success/partial_success, 1 otherwise.
#[cfg(test)]
pub(crate) async fn attach_run(
    run_dir: &Path,
    storage_dir: Option<&Path>,
    run_id: Option<&RunId>,
    kill_on_detach: bool,
    styles: &'static Styles,
    json_output: bool,
) -> Result<ExitCode> {
    let inferred_storage_dir = infer_storage_dir(run_dir);
    let inferred_run_id = infer_run_id(run_dir);
    let storage_dir = storage_dir.map(Path::to_path_buf).or(inferred_storage_dir);
    let run_id = run_id.copied().or(inferred_run_id);

    if let (Some(storage_dir), Some(run_id)) = (storage_dir.as_deref(), run_id.as_ref()) {
        let client = server_client::connect_server(storage_dir).await?;
        return attach_run_with_client(
            &client,
            run_id,
            kill_on_detach,
            styles,
            json_output,
            Printer::Default,
        )
        .await;
    }

    Err(anyhow::anyhow!(
        "Could not infer SlateDB storage location and run id for attach"
    ))
}

pub(crate) async fn attach_run_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    kill_on_detach: bool,
    styles: &'static Styles,
    json_output: bool,
    printer: Printer,
) -> Result<ExitCode> {
    let state = client.get_run_state(run_id).await?;
    let auto_approve = state.run.as_ref().is_some_and(|record| {
        fabro_config::resolve_run_from_file(&record.settings)
            .map(|settings| settings.execution.approval == ApprovalMode::Auto)
            .unwrap_or(false)
    });
    let verbose = state.run.as_ref().is_some_and(|record| {
        fabro_config::resolve_cli_from_file(&record.settings)
            .map(|settings| settings.output.verbosity == OutputVerbosity::Verbose)
            .unwrap_or(false)
    });
    let events = client.list_run_events(run_id, None, None).await?;
    let replay_events = events.clone();
    let next_seq = events.last().map_or(1, |event| event.seq.saturating_add(1));
    let initial_exit_code = events.iter().rev().find_map(event_exit_code);
    let state_exit_code = state_exit_code(&state);

    if state_is_terminal(&state) || initial_exit_code.is_some() {
        return replay_run_with_client(
            verbose,
            events,
            initial_exit_code
                .or(state_exit_code)
                .unwrap_or(ExitCode::from(1)),
            json_output,
        );
    }

    let stream = client.attach_run_events(run_id, Some(next_seq)).await?;
    attach_live_run_with_client(
        client,
        run_id,
        replay_events,
        stream,
        styles,
        AttachOptions {
            auto_approve,
            verbose,
            kill_on_detach,
            json_output,
        },
        printer,
    )
    .await
}

struct AttachOptions {
    auto_approve: bool,
    verbose: bool,
    kill_on_detach: bool,
    json_output: bool,
}

fn replay_run_with_client(
    verbose: bool,
    events: Vec<EventEnvelope>,
    exit_code: ExitCode,
    json_output: bool,
) -> Result<ExitCode> {
    let is_tty = std::io::stderr().is_terminal();
    let mut progress_ui = run_progress::ProgressUI::new(is_tty, verbose);

    for event in events {
        let line = event_payload_line(&event)?;
        emit_progress_line(&mut progress_ui, &line, json_output)?;
    }

    finish_progress(&mut progress_ui, json_output);

    Ok(exit_code)
}

async fn attach_live_run_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    existing_events: Vec<EventEnvelope>,
    mut stream: server_client::RunAttachEventStream,
    styles: &'static Styles,
    opts: AttachOptions,
    printer: Printer,
) -> Result<ExitCode> {
    let is_tty = std::io::stderr().is_terminal();
    let mut progress_ui = run_progress::ProgressUI::new(is_tty, opts.verbose);
    let ctrl_c_signal = ctrl_c();
    tokio::pin!(ctrl_c_signal);

    for event in existing_events {
        let line = event_payload_line(&event)?;
        emit_progress_line(&mut progress_ui, &line, opts.json_output)?;
    }

    if let Some(exit_code) = handle_pending_server_interview(
        client,
        run_id,
        opts.auto_approve,
        &mut progress_ui,
        styles,
        opts.json_output,
        printer,
    )
    .await?
    {
        return Ok(exit_code);
    }

    loop {
        let next_event = tokio::select! {
            _ = &mut ctrl_c_signal => {
                handle_detach_signal(client, run_id, opts.kill_on_detach, printer).await;
                finish_progress(&mut progress_ui, opts.json_output);
                return Ok(ExitCode::from(1));
            }
            result = stream.next_event() => result?,
        };

        let Some(event) = next_event else {
            finish_progress(&mut progress_ui, opts.json_output);
            return Err(anyhow::anyhow!(ATTACH_PREMATURE_EOF_MESSAGE));
        };

        let line = event_payload_line(&event)?;
        emit_progress_line(&mut progress_ui, &line, opts.json_output)?;

        if let Some(exit_code) = event_exit_code(&event) {
            finish_progress(&mut progress_ui, opts.json_output);
            return Ok(exit_code);
        }

        if event_starts_interview(&event) {
            if let Some(exit_code) = handle_pending_server_interview(
                client,
                run_id,
                opts.auto_approve,
                &mut progress_ui,
                styles,
                opts.json_output,
                printer,
            )
            .await?
            {
                return Ok(exit_code);
            }
        }
    }
}

async fn handle_pending_server_interview(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    auto_approve: bool,
    progress_ui: &mut run_progress::ProgressUI,
    styles: &'static Styles,
    json_output: bool,
    printer: Printer,
) -> Result<Option<ExitCode>> {
    let Some(question) = client.list_run_questions(run_id).await?.into_iter().next() else {
        return Ok(None);
    };

    if json_pending_interview_requires_manual_input(json_output, auto_approve) {
        fabro_util::printerr!(printer, "{JSON_INTERVIEW_MESSAGE}");
        return Ok(Some(ExitCode::from(1)));
    }
    if json_output {
        return Ok(None);
    }

    hide_progress(progress_ui, json_output);
    let interviewer = ConsoleInterviewer::new(styles);
    let answer =
        fabro_interview::Interviewer::ask(&interviewer, api_question_to_question(&question)).await;
    show_progress(progress_ui, json_output);

    if answer_requires_reattach(&answer) {
        fabro_util::printerr!(printer, "{INTERVIEW_UNANSWERED_MESSAGE}");
        return Ok(Some(ExitCode::from(1)));
    }

    submit_server_interview_answer(client, run_id, &question.id, &answer).await?;
    Ok(None)
}

async fn handle_detach_signal(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    kill_on_detach: bool,
    printer: Printer,
) {
    if kill_on_detach {
        let _ = client.cancel_run(run_id).await;
        for _ in 0..20 {
            if client
                .get_run_state(run_id)
                .await
                .ok()
                .is_some_and(|state| state_is_terminal(&state))
            {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    } else {
        fabro_util::printerr!(
            printer,
            "Detached from run (engine continues in background)"
        );
    }
}

fn api_question_to_question(question: &types::ApiQuestion) -> Question {
    let question_type = match question.question_type {
        types::QuestionType::YesNo => QuestionType::YesNo,
        types::QuestionType::MultipleChoice => QuestionType::MultipleChoice,
        types::QuestionType::MultiSelect => QuestionType::MultiSelect,
        types::QuestionType::Freeform => QuestionType::Freeform,
        types::QuestionType::Confirmation => QuestionType::Confirmation,
    };
    let mut converted = Question::new(question.text.clone(), question_type);
    converted.id.clone_from(&question.id);
    converted.options = question
        .options
        .iter()
        .map(|option| QuestionOption {
            key: option.key.clone(),
            label: option.label.clone(),
        })
        .collect();
    converted.allow_freeform = question.allow_freeform;
    converted.stage.clone_from(&question.stage);
    converted.timeout_seconds = question.timeout_seconds;
    converted
        .context_display
        .clone_from(&question.context_display);
    converted
}

async fn submit_server_interview_answer(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    qid: &str,
    answer: &fabro_interview::Answer,
) -> Result<bool> {
    let (value, selected_option_key, selected_option_keys) = match &answer.value {
        AnswerValue::Text(text) => (Some(text.clone()), None, Vec::new()),
        AnswerValue::Selected(key) => (None, Some(key.clone()), Vec::new()),
        AnswerValue::MultiSelected(keys) => (None, None, keys.clone()),
        AnswerValue::Yes => (Some("yes".to_string()), None, Vec::new()),
        AnswerValue::No => (Some("no".to_string()), None, Vec::new()),
        AnswerValue::Cancelled
        | AnswerValue::Interrupted
        | AnswerValue::Skipped
        | AnswerValue::Timeout => {
            return Ok(false);
        }
    };
    client
        .submit_run_answer(
            run_id,
            qid,
            value,
            selected_option_key,
            selected_option_keys,
        )
        .await?;
    Ok(true)
}

fn json_pending_interview_requires_manual_input(json_output: bool, auto_approve: bool) -> bool {
    json_output && !auto_approve
}

fn state_is_terminal(state: &server_client::RunProjection) -> bool {
    state.conclusion.is_some()
        || state
            .status
            .as_ref()
            .is_some_and(|record| record.status.is_terminal())
}

fn emit_progress_line(
    progress_ui: &mut run_progress::ProgressUI,
    line: &str,
    json_output: bool,
) -> Result<()> {
    if json_output {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        writeln!(handle, "{line}")?;
    } else {
        progress_ui.handle_json_line(line);
    }
    Ok(())
}

fn finish_progress(progress_ui: &mut run_progress::ProgressUI, json_output: bool) {
    if !json_output {
        progress_ui.finish();
    }
}

fn hide_progress(progress_ui: &mut run_progress::ProgressUI, json_output: bool) {
    if !json_output {
        progress_ui.hide_bars();
    }
}

fn show_progress(progress_ui: &mut run_progress::ProgressUI, json_output: bool) {
    if !json_output {
        progress_ui.show_bars();
    }
}

fn event_payload_line(event: &EventEnvelope) -> Result<String> {
    let mut value = normalize_json_value(event.payload.as_value().clone());
    restore_empty_run_properties(&mut value);
    serde_json::to_string(&value).map_err(Into::into)
}

fn restore_empty_run_properties(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(event_name) = object.get("event").and_then(serde_json::Value::as_str) else {
        return;
    };
    if matches!(event_name, "run.submitted" | "run.running") && !object.contains_key("properties") {
        let run_id = object.remove("run_id");
        let ts = object.remove("ts");
        object.insert("properties".to_string(), serde_json::json!({}));
        if let Some(run_id) = run_id {
            object.insert("run_id".to_string(), run_id);
        }
        if let Some(ts) = ts {
            object.insert("ts".to_string(), ts);
        }
    }
}

#[cfg(test)]
fn infer_storage_dir(run_dir: &Path) -> Option<PathBuf> {
    let scratch_dir = run_dir.parent()?;
    let storage_dir = scratch_dir.parent()?;
    (scratch_dir.file_name()? == "scratch").then(|| storage_dir.to_path_buf())
}

#[cfg(test)]
fn infer_run_id(run_dir: &Path) -> Option<RunId> {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| name.rsplit('-').next().map(ToOwned::to_owned))
        .filter(|run_id| !run_id.is_empty())
        .and_then(|run_id| run_id.parse().ok())
}

fn answer_requires_reattach(answer: &fabro_interview::Answer) -> bool {
    matches!(
        answer.value,
        AnswerValue::Interrupted | AnswerValue::Skipped
    )
}

fn state_exit_code(state: &server_client::RunProjection) -> Option<ExitCode> {
    if let Some(conclusion) = &state.conclusion {
        let success = matches!(
            conclusion.status,
            StageStatus::Success | StageStatus::PartialSuccess
        );
        return Some(if success {
            ExitCode::from(0)
        } else {
            ExitCode::from(1)
        });
    }

    match state.status.as_ref() {
        Some(record) if record.status == RunStatus::Succeeded => Some(ExitCode::from(0)),
        Some(record) if record.status.is_terminal() => Some(ExitCode::from(1)),
        Some(_) | None => None,
    }
}

fn event_exit_code(event: &EventEnvelope) -> Option<ExitCode> {
    let run_event = RunEvent::try_from(&event.payload).ok()?;
    match run_event.body {
        EventBody::RunCompleted(props) => Some(
            if props.status == "success" || props.status == "partial_success" {
                ExitCode::from(0)
            } else {
                ExitCode::from(1)
            },
        ),
        EventBody::RunFailed(_) => Some(ExitCode::from(1)),
        _ => None,
    }
}

fn event_starts_interview(event: &EventEnvelope) -> bool {
    let Ok(run_event) = RunEvent::try_from(&event.payload) else {
        return false;
    };
    matches!(run_event.body, EventBody::InterviewStarted(_))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::absolute_paths)]

    use fabro_interview::{Answer, AnswerValue};
    use fabro_util::terminal::Styles;
    use httpmock::MockServer;

    use super::*;

    fn no_color_styles() -> &'static Styles {
        Box::leak(Box::new(Styles::new(false)))
    }

    fn terminal_run_state_response() -> serde_json::Value {
        serde_json::json!({
            "run": null,
            "graph_source": null,
            "start": null,
            "status": {
                "status": "failed",
                "reason": "cancelled",
                "updated_at": "2026-04-05T12:00:02Z"
            },
            "checkpoint": null,
            "checkpoints": [],
            "conclusion": null,
            "retro": null,
            "retro_prompt": null,
            "retro_response": null,
            "sandbox": null,
            "final_patch": null,
            "pull_request": null,
            "nodes": {}
        })
    }

    fn cancel_run_response(run_id: RunId) -> serde_json::Value {
        serde_json::json!({
            "id": run_id,
            "status": "cancelled",
            "error": null,
            "queue_position": null,
            "status_reason": "cancelled",
            "pending_control": "cancel",
            "created_at": "2026-04-05T12:00:00Z"
        })
    }

    #[tokio::test]
    async fn attach_errors_without_store_context() {
        let dir = tempfile::tempdir().unwrap();

        let err = attach_run(dir.path(), None, None, false, no_color_styles(), false)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("Could not infer SlateDB storage location and run id for attach")
        );
    }

    #[test]
    fn infer_storage_dir_detects_standard_run_layout() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir
            .path()
            .join("storage")
            .join("scratch")
            .join("20260401-test");
        std::fs::create_dir_all(&run_dir).unwrap();

        assert_eq!(
            infer_storage_dir(&run_dir),
            Some(dir.path().join("storage"))
        );
    }

    #[test]
    fn infer_run_id_reads_run_dir_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_id = fabro_types::fixtures::RUN_1;
        let run_dir = storage_dir
            .join("scratch")
            .join(format!("20260401-{run_id}"));
        std::fs::create_dir_all(&run_dir).unwrap();

        assert_eq!(infer_run_id(&run_dir), Some(run_id));
    }

    #[test]
    fn answer_requires_reattach_for_interrupted_and_skipped_answers() {
        let interrupted = Answer {
            value: AnswerValue::Interrupted,
            selected_option: None,
            text: None,
        };
        let skipped = Answer {
            value: AnswerValue::Skipped,
            selected_option: None,
            text: None,
        };
        let answered = Answer::yes();

        assert!(answer_requires_reattach(&interrupted));
        assert!(answer_requires_reattach(&skipped));
        assert!(!answer_requires_reattach(&answered));
    }

    #[test]
    fn json_pending_interview_requires_manual_input_when_auto_approve_is_disabled() {
        assert!(json_pending_interview_requires_manual_input(true, false));
    }

    #[test]
    fn json_pending_interview_does_not_require_manual_input_when_auto_approve_is_enabled() {
        assert!(!json_pending_interview_requires_manual_input(true, true));
    }

    #[tokio::test]
    async fn handle_detach_signal_with_kill_on_detach_cancels_active_run_via_server() {
        let run_id = fabro_types::fixtures::RUN_1;
        let server = MockServer::start();
        let cancel_mock = server.mock(|when, then| {
            when.method("POST")
                .path(format!("/api/v1/runs/{run_id}/cancel"));
            then.status(200)
                .header("Content-Type", "application/json")
                .body(cancel_run_response(run_id).to_string());
        });
        let state_mock = server.mock(|when, then| {
            when.method("GET")
                .path(format!("/api/v1/runs/{run_id}/state"));
            then.status(200)
                .header("Content-Type", "application/json")
                .body(terminal_run_state_response().to_string());
        });
        let client = server_client::ServerStoreClient::new_no_proxy(&server.base_url()).unwrap();

        handle_detach_signal(&client, &run_id, true, Printer::Default).await;

        cancel_mock.assert();
        state_mock.assert();
    }
}
