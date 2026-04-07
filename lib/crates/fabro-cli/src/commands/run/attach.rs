use std::io::{IsTerminal, Write};
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Result;
use fabro_types::{EventBody, RunEvent, RunId};

use fabro_api::types;
use fabro_interview::{AnswerValue, ConsoleInterviewer, Question, QuestionOption, QuestionType};
use fabro_store::EventEnvelope;
use fabro_util::json::normalize_json_value;
use fabro_util::terminal::Styles;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::run_status::RunStatus;
use tokio::signal::ctrl_c;
use tokio::time::{sleep, timeout};

use super::run_progress;
use crate::server_client;

const INTERVIEW_UNANSWERED_MESSAGE: &str =
    "Interview ended without an answer. The run is still waiting for input; reattach to answer it.";
const JSON_INTERVIEW_MESSAGE: &str = "This run is waiting for human input, but --json is non-interactive. Reattach without --json to answer it.";
#[cfg(test)]
const ATTACH_FINAL_STATUS_GRACE: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const ATTACH_FINAL_STATUS_GRACE: Duration = Duration::from_secs(2);

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
        return attach_run_with_client(&client, run_id, kill_on_detach, styles, json_output).await;
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
) -> Result<ExitCode> {
    let state = client.get_run_state(run_id).await?;
    let verbose = state
        .run
        .as_ref()
        .is_some_and(|record| record.settings.verbose_enabled());
    let events = client.list_run_events(run_id, None, None).await?;
    let replay_events = events.clone();
    let next_seq = events.last().map_or(1, |event| event.seq.saturating_add(1));
    let initial_exit_code = events.iter().rev().find_map(event_exit_code);

    if state_is_terminal(&state) || initial_exit_code.is_some() {
        return replay_run_with_client(client, run_id, verbose, events, json_output).await;
    }

    match client.attach_run_events(run_id, Some(next_seq)).await {
        Ok(stream) => {
            attach_live_run_with_client(
                client,
                run_id,
                verbose,
                events,
                stream,
                kill_on_detach,
                styles,
                json_output,
            )
            .await
        }
        Err(server_client::RunAttachStreamError::Gone) => {
            replay_run_with_client(client, run_id, verbose, replay_events, json_output).await
        }
        Err(server_client::RunAttachStreamError::Other(err)) => Err(err),
    }
}

async fn replay_run_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    verbose: bool,
    events: Vec<EventEnvelope>,
    json_output: bool,
) -> Result<ExitCode> {
    let is_tty = std::io::stderr().is_terminal();
    let mut progress_ui = run_progress::ProgressUI::new(is_tty, verbose);
    let mut terminal_exit_code = None;

    for event in events {
        if let Some(exit_code) = event_exit_code(&event) {
            terminal_exit_code = Some(exit_code);
        }
        let line = event_payload_line(&event)?;
        emit_progress_line(&mut progress_ui, &line, json_output)?;
    }

    finish_progress(&mut progress_ui, json_output);

    Ok(match terminal_exit_code {
        Some(exit_code) => exit_code,
        None => determine_exit_code_with_server(client, run_id).await,
    })
}

async fn attach_live_run_with_client(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    verbose: bool,
    existing_events: Vec<EventEnvelope>,
    mut stream: server_client::RunAttachEventStream,
    kill_on_detach: bool,
    styles: &'static Styles,
    json_output: bool,
) -> Result<ExitCode> {
    let is_tty = std::io::stderr().is_terminal();
    let mut progress_ui = run_progress::ProgressUI::new(is_tty, verbose);
    let ctrl_c_signal = ctrl_c();
    tokio::pin!(ctrl_c_signal);

    let mut next_seq = 1;
    let mut terminal_exit_code = None;
    let mut terminal_event_seen_at: Option<Instant> = None;

    for event in existing_events {
        next_seq = event.seq.saturating_add(1);
        if let Some(exit_code) = event_exit_code(&event) {
            terminal_exit_code = Some(exit_code);
            terminal_event_seen_at = Some(Instant::now());
        }
        let line = event_payload_line(&event)?;
        emit_progress_line(&mut progress_ui, &line, json_output)?;
    }

    if let Some(exit_code) =
        handle_pending_server_interview(client, run_id, &mut progress_ui, styles, json_output)
            .await?
    {
        return Ok(exit_code);
    }

    loop {
        let next_event = if let Some(seen_at) = terminal_event_seen_at {
            let remaining = ATTACH_FINAL_STATUS_GRACE.saturating_sub(seen_at.elapsed());
            if remaining.is_zero() {
                break;
            }
            tokio::select! {
                _ = &mut ctrl_c_signal => {
                    handle_detach_signal(client, run_id, kill_on_detach).await;
                    break;
                }
                result = timeout(remaining, stream.next_event()) => {
                    match result {
                        Ok(result) => result?,
                        Err(_) => break,
                    }
                }
            }
        } else {
            tokio::select! {
                _ = &mut ctrl_c_signal => {
                    handle_detach_signal(client, run_id, kill_on_detach).await;
                    break;
                }
                result = stream.next_event() => result?,
            }
        };

        let Some(event) = next_event else {
            break;
        };

        next_seq = event.seq.saturating_add(1);
        if let Some(exit_code) = event_exit_code(&event) {
            terminal_exit_code = Some(exit_code);
            terminal_event_seen_at = Some(Instant::now());
        }

        let line = event_payload_line(&event)?;
        emit_progress_line(&mut progress_ui, &line, json_output)?;

        if event_starts_interview(&event) {
            if let Some(exit_code) = handle_pending_server_interview(
                client,
                run_id,
                &mut progress_ui,
                styles,
                json_output,
            )
            .await?
            {
                return Ok(exit_code);
            }
        }
    }

    if terminal_exit_code.is_none() {
        let (_, trailing_exit_code) =
            emit_server_events_from(client, run_id, next_seq, &mut progress_ui, json_output)
                .await?;
        terminal_exit_code = trailing_exit_code;
    }

    finish_progress(&mut progress_ui, json_output);

    Ok(match terminal_exit_code {
        Some(exit_code) => exit_code,
        None => determine_exit_code_with_server(client, run_id).await,
    })
}

async fn handle_pending_server_interview(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    progress_ui: &mut run_progress::ProgressUI,
    styles: &'static Styles,
    json_output: bool,
) -> Result<Option<ExitCode>> {
    let Some(question) = client.list_run_questions(run_id).await?.into_iter().next() else {
        return Ok(None);
    };

    if json_output {
        eprintln!("{JSON_INTERVIEW_MESSAGE}");
        return Ok(Some(ExitCode::from(1)));
    }

    hide_progress(progress_ui, json_output);
    let interviewer = ConsoleInterviewer::new(styles);
    let answer =
        fabro_interview::Interviewer::ask(&interviewer, api_question_to_question(&question)).await;
    show_progress(progress_ui, json_output);

    if answer_requires_reattach(&answer) {
        eprintln!("{INTERVIEW_UNANSWERED_MESSAGE}");
        return Ok(Some(ExitCode::from(1)));
    }

    submit_server_interview_answer(client, run_id, &question.id, &answer).await?;
    Ok(None)
}

async fn handle_detach_signal(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    kill_on_detach: bool,
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
        eprintln!("Detached from run (engine continues in background)");
    }
}

async fn emit_server_events_from(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
    next_seq: u32,
    progress_ui: &mut run_progress::ProgressUI,
    json_output: bool,
) -> Result<(u32, Option<ExitCode>)> {
    let events = match client.list_run_events(run_id, Some(next_seq), None).await {
        Ok(events) => events,
        Err(err) if is_run_not_found_error(&err) => Vec::new(),
        Err(err) => return Err(err),
    };

    let mut current_seq = next_seq;
    let mut terminal_exit_code = None;
    for event in events {
        if let Some(exit_code) = event_exit_code(&event) {
            terminal_exit_code = Some(exit_code);
        }
        let line = event_payload_line(&event)?;
        emit_progress_line(progress_ui, &line, json_output)?;
        current_seq = event.seq.saturating_add(1);
    }

    Ok((current_seq, terminal_exit_code))
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
    converted.options = question
        .options
        .iter()
        .map(|option| QuestionOption {
            key: option.key.clone(),
            label: option.label.clone(),
        })
        .collect();
    converted.allow_freeform = question.allow_freeform;
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
        AnswerValue::Aborted | AnswerValue::Skipped | AnswerValue::Timeout => {
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

fn is_run_not_found_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string() == "Run not found.")
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
    std::fs::read_to_string(run_dir.join("id.txt"))
        .ok()
        .map(|run_id| run_id.trim().to_string())
        .filter(|run_id| !run_id.is_empty())
        .and_then(|run_id| run_id.parse().ok())
}

fn answer_requires_reattach(answer: &fabro_interview::Answer) -> bool {
    matches!(answer.value, AnswerValue::Aborted | AnswerValue::Skipped)
}

async fn determine_exit_code_with_server(
    client: &server_client::ServerStoreClient,
    run_id: &RunId,
) -> ExitCode {
    let deadline = Instant::now() + ATTACH_FINAL_STATUS_GRACE;
    loop {
        if let Ok(state) = client.get_run_state(run_id).await {
            if let Some(conclusion) = state.conclusion {
                let success = matches!(
                    conclusion.status,
                    StageStatus::Success | StageStatus::PartialSuccess
                );
                return if success {
                    ExitCode::from(0)
                } else {
                    ExitCode::from(1)
                };
            }

            match state.status {
                Some(record) if matches!(record.status, RunStatus::Succeeded) => {
                    return ExitCode::from(0);
                }
                Some(record) if record.status.is_terminal() => return ExitCode::from(1),
                Some(_) | None => {}
            }
        }

        if Instant::now() >= deadline {
            return ExitCode::from(1);
        }
        sleep(Duration::from_millis(100)).await;
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

    use super::*;
    use fabro_interview::{Answer, AnswerValue};
    use fabro_util::terminal::Styles;

    fn no_color_styles() -> &'static Styles {
        Box::leak(Box::new(Styles::new(false)))
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
    fn infer_run_id_reads_id_txt() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_dir = storage_dir.join("scratch").join("20260401-test");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("id.txt"),
            format!("{}\n", fabro_types::fixtures::RUN_1),
        )
        .unwrap();

        assert_eq!(infer_run_id(&run_dir), Some(fabro_types::fixtures::RUN_1));
    }

    #[test]
    fn answer_requires_reattach_for_aborted_and_skipped_answers() {
        let aborted = Answer {
            value: AnswerValue::Aborted,
            selected_option: None,
            text: None,
        };
        let skipped = Answer {
            value: AnswerValue::Skipped,
            selected_option: None,
            text: None,
        };
        let answered = Answer::yes();

        assert!(answer_requires_reattach(&aborted));
        assert!(answer_requires_reattach(&skipped));
        assert!(!answer_requires_reattach(&answered));
    }
}
