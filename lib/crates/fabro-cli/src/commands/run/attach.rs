#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `run attach` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI `run attach` command: writes to std::io::stdout/stderr directly"
)]

use std::io::{IsTerminal, Write};
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use fabro_api::types;
use fabro_interview::{Answer, AnswerValue, Question};
use fabro_store::EventEnvelope;
use fabro_types::settings::run::ApprovalMode;
use fabro_types::{EventBody, InterviewOption, QuestionType, RunId};
use fabro_util::json::normalize_json_value;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use fabro_workflow::outcome::StageOutcome;
use fabro_workflow::run_status::RunStatus;
use tokio::signal::ctrl_c;
use tokio::time::{Duration as TokioDuration, sleep};

use super::run_progress;
use crate::server_client;

const INTERVIEW_UNANSWERED_MESSAGE: &str =
    "Interview ended without an answer. The run is still waiting for input; reattach to answer it.";
const JSON_INTERVIEW_MESSAGE: &str = "This run is waiting for human input, but --json is non-interactive. Reattach without --json to answer it.";
const ATTACH_PREMATURE_EOF_MESSAGE: &str = "Attach stream ended before terminal run event.";
const PROMPT_READ_POLL_INTERVAL: TokioDuration = TokioDuration::from_millis(50);

enum PromptRead {
    Line(String),
    Eof,
    Error,
}

enum ParsedPromptAnswer {
    Answer(Answer),
    Invalid(String),
    Interrupted,
}

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::fcntl::{FcntlArg, OFlag, fcntl};
#[cfg(unix)]
use nix::unistd;
#[cfg(unix)]
enum LineRead {
    Pending,
    Complete(String),
    Eof,
    Error,
}

#[cfg(unix)]
struct NonblockingStdin {
    stdin:          std::io::Stdin,
    original_flags: OFlag,
}

#[cfg(unix)]
impl NonblockingStdin {
    fn new() -> Option<Self> {
        let stdin = std::io::stdin();
        let original_flags =
            OFlag::from_bits_truncate(fcntl(stdin.as_fd(), FcntlArg::F_GETFL).ok()?);
        fcntl(
            stdin.as_fd(),
            FcntlArg::F_SETFL(original_flags | OFlag::O_NONBLOCK),
        )
        .ok()?;
        Some(Self {
            stdin,
            original_flags,
        })
    }

    fn read_line(&self, buffer: &mut Vec<u8>) -> LineRead {
        let mut chunk = [0_u8; 256];
        loop {
            if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.drain(..=newline).collect::<Vec<_>>();
                return LineRead::Complete(
                    String::from_utf8_lossy(&line)
                        .trim_end_matches(['\r', '\n'])
                        .to_string(),
                );
            }
            match unistd::read(self.stdin.as_fd(), &mut chunk) {
                Ok(0) => {
                    return if buffer.is_empty() {
                        LineRead::Eof
                    } else {
                        let line = std::mem::take(buffer);
                        LineRead::Complete(String::from_utf8_lossy(&line).to_string())
                    };
                }
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line = buffer.drain(..=newline).collect::<Vec<_>>();
                        return LineRead::Complete(
                            String::from_utf8_lossy(&line)
                                .trim_end_matches(['\r', '\n'])
                                .to_string(),
                        );
                    }
                }
                Err(Errno::EAGAIN) => return LineRead::Pending,
                Err(_) => return LineRead::Error,
            }
        }
    }
}

#[cfg(unix)]
impl Drop for NonblockingStdin {
    fn drop(&mut self) {
        let _ = fcntl(self.stdin.as_fd(), FcntlArg::F_SETFL(self.original_flags));
    }
}

/// Attach to a running (or finished) workflow run, rendering progress live.
///
/// Returns exit code 0 for succeeded/partially_succeeded, 1 otherwise.
#[cfg(test)]
pub(crate) async fn attach_run(
    run_dir: &Path,
    storage_dir: Option<&Path>,
    run_id: Option<&RunId>,
    kill_on_detach: bool,
    styles: &'static Styles,
    json_output: bool,
    live_verbose: bool,
) -> Result<ExitCode> {
    let inferred_storage_dir = infer_storage_dir(run_dir);
    let inferred_run_id = infer_run_id(run_dir);
    let storage_dir = storage_dir.map(Path::to_path_buf).or(inferred_storage_dir);
    let run_id = run_id.copied().or(inferred_run_id);

    if let (Some(storage_dir), Some(run_id)) = (storage_dir.as_deref(), run_id.as_ref()) {
        let client = server_client::connect_server(storage_dir).await?;
        return Box::pin(attach_run_with_client(
            &client,
            run_id,
            kill_on_detach,
            styles,
            json_output,
            live_verbose,
            Printer::Default,
        ))
        .await;
    }

    Err(anyhow::anyhow!(
        "Could not infer SlateDB storage location and run id for attach"
    ))
}

pub(crate) async fn attach_run_with_client(
    client: &server_client::Client,
    run_id: &RunId,
    kill_on_detach: bool,
    styles: &'static Styles,
    json_output: bool,
    live_verbose: bool,
    printer: Printer,
) -> Result<ExitCode> {
    let state = client.get_run_state(run_id).await?;
    let auto_approve = state
        .spec
        .as_ref()
        .is_some_and(|record| record.settings.run.execution.approval == ApprovalMode::Auto);
    let events = client.list_run_events(run_id, None, None).await?;
    let replay_events = events.clone();
    let next_seq = events.last().map_or(1, |event| event.seq.saturating_add(1));
    let initial_exit_code = events.iter().rev().find_map(event_exit_code);
    let state_exit_code = state_exit_code(&state);

    if state_is_terminal(&state) || initial_exit_code.is_some() {
        return replay_run_with_client(
            live_verbose,
            events,
            initial_exit_code
                .or(state_exit_code)
                .unwrap_or(ExitCode::from(1)),
            json_output,
        );
    }

    let stream = client.attach_run_events(run_id, Some(next_seq)).await?;
    Box::pin(attach_live_run_with_client(
        client,
        run_id,
        replay_events,
        stream,
        styles,
        AttachOptions {
            auto_approve,
            verbose: live_verbose,
            kill_on_detach,
            json_output,
        },
        printer,
    ))
    .await
}

struct AttachOptions {
    auto_approve:   bool,
    verbose:        bool,
    kill_on_detach: bool,
    json_output:    bool,
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
    client: &server_client::Client,
    run_id: &RunId,
    existing_events: Vec<EventEnvelope>,
    mut stream: server_client::RunEventStream,
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

    if let Some(exit_code) = Box::pin(handle_pending_server_interview(
        client,
        run_id,
        &mut stream,
        opts.auto_approve,
        &mut progress_ui,
        styles,
        opts.json_output,
        opts.kill_on_detach,
        printer,
    ))
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
            if let Some(exit_code) = Box::pin(handle_pending_server_interview(
                client,
                run_id,
                &mut stream,
                opts.auto_approve,
                &mut progress_ui,
                styles,
                opts.json_output,
                opts.kill_on_detach,
                printer,
            ))
            .await?
            {
                return Ok(exit_code);
            }
        }
    }
}

async fn handle_pending_server_interview(
    client: &server_client::Client,
    run_id: &RunId,
    stream: &mut server_client::RunEventStream,
    auto_approve: bool,
    progress_ui: &mut run_progress::ProgressUI,
    styles: &'static Styles,
    json_output: bool,
    kill_on_detach: bool,
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
    let ask = ask_attach_question(api_question_to_question(&question), styles);
    tokio::pin!(ask);
    let ctrl_c_signal = ctrl_c();
    tokio::pin!(ctrl_c_signal);

    let answer = loop {
        let next_event = tokio::select! {
            answer = &mut ask => {
                break answer;
            }
            _ = &mut ctrl_c_signal => {
                handle_detach_signal(client, run_id, kill_on_detach, printer).await;
                show_progress(progress_ui, json_output);
                return Ok(Some(ExitCode::from(1)));
            }
            result = stream.next_event() => result?,
        };

        let Some(event) = next_event else {
            show_progress(progress_ui, json_output);
            return Err(anyhow::anyhow!(ATTACH_PREMATURE_EOF_MESSAGE));
        };

        let line = event_payload_line(&event)?;
        emit_progress_line(progress_ui, &line, json_output)?;

        if let Some(exit_code) = event_exit_code(&event) {
            show_progress(progress_ui, json_output);
            return Ok(Some(exit_code));
        }

        if event_resolves_interview(&event, &question.id) {
            show_progress(progress_ui, json_output);
            return Ok(None);
        }
    };
    show_progress(progress_ui, json_output);

    if answer_requires_reattach(&answer) {
        fabro_util::printerr!(printer, "{INTERVIEW_UNANSWERED_MESSAGE}");
        return Ok(Some(ExitCode::from(1)));
    }

    submit_server_interview_answer(client, run_id, &question.id, &answer).await?;
    Ok(None)
}

async fn handle_detach_signal(
    client: &server_client::Client,
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
    let mut converted = Question::new(question.text.clone(), question.question_type);
    converted.id.clone_from(&question.id);
    converted.options = question
        .options
        .iter()
        .map(|option| InterviewOption {
            key:   option.key.clone(),
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

#[allow(
    clippy::print_stderr,
    reason = "Interactive questions and options belong on stderr, not captured stdout."
)]
async fn ask_attach_question(question: Question, styles: &'static Styles) -> Answer {
    let mut input_buffer = Vec::new();
    if let Some(ref context_text) = question.context_display {
        let rendered = styles.render_markdown(context_text);
        eprint!("{rendered}");
    }
    eprintln!("{} {}", styles.bold_cyan.apply_to("?"), question.text);

    match question.question_type {
        QuestionType::MultipleChoice | QuestionType::MultiSelect => {
            for (i, opt) in question.options.iter().enumerate() {
                eprintln!(
                    "  {}{}{}  {} - {}",
                    styles.dim.apply_to("["),
                    styles.bold.apply_to(i + 1),
                    styles.dim.apply_to("]"),
                    opt.key,
                    opt.label,
                );
            }
            if question.allow_freeform {
                eprintln!("  Or type a free-text response");
            }
            loop {
                match parse_choice_response(
                    &question,
                    read_attach_line("Select: ", &mut input_buffer).await,
                ) {
                    ParsedPromptAnswer::Answer(answer) => return answer,
                    ParsedPromptAnswer::Invalid(message) => eprintln!("{message}"),
                    ParsedPromptAnswer::Interrupted => return Answer::interrupted(),
                }
            }
        }
        QuestionType::YesNo | QuestionType::Confirmation => loop {
            match parse_confirm_response(read_attach_line("[Y/N]: ", &mut input_buffer).await) {
                ParsedPromptAnswer::Answer(answer) => return answer,
                ParsedPromptAnswer::Invalid(message) => eprintln!("{message}"),
                ParsedPromptAnswer::Interrupted => return Answer::interrupted(),
            }
        },
        QuestionType::Freeform => loop {
            match parse_freeform_response(read_attach_line("> ", &mut input_buffer).await) {
                ParsedPromptAnswer::Answer(answer) => return answer,
                ParsedPromptAnswer::Invalid(message) => eprintln!("{message}"),
                ParsedPromptAnswer::Interrupted => return Answer::interrupted(),
            }
        },
    }
}

#[allow(
    clippy::print_stderr,
    reason = "Prompts go to stderr so piped stdout stays machine-readable."
)]
async fn read_attach_line(prompt: &str, buffer: &mut Vec<u8>) -> PromptRead {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    read_attach_line_after_prompt(buffer).await
}

#[cfg(unix)]
async fn read_attach_line_after_prompt(buffer: &mut Vec<u8>) -> PromptRead {
    let Some(stdin) = NonblockingStdin::new() else {
        return PromptRead::Error;
    };
    loop {
        match stdin.read_line(buffer) {
            LineRead::Pending => sleep(PROMPT_READ_POLL_INTERVAL).await,
            LineRead::Complete(line) => return PromptRead::Line(line),
            LineRead::Eof => return PromptRead::Eof,
            LineRead::Error => return PromptRead::Error,
        }
    }
}

#[cfg(not(unix))]
async fn read_attach_line_after_prompt(_buffer: &mut Vec<u8>) -> PromptRead {
    use tokio::io::{self, AsyncBufReadExt, BufReader};

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => PromptRead::Eof,
        Ok(_) => PromptRead::Line(line.trim_end().to_string()),
        Err(_) => PromptRead::Error,
    }
}

fn parse_choice_response(question: &Question, prompt_read: PromptRead) -> ParsedPromptAnswer {
    let PromptRead::Line(response) = prompt_read else {
        return ParsedPromptAnswer::Interrupted;
    };
    if response.trim().is_empty() {
        return ParsedPromptAnswer::Invalid(invalid_choice_message(question));
    }
    if question.question_type == QuestionType::MultiSelect {
        let selected = response
            .split([',', ' '])
            .filter(|part| !part.trim().is_empty())
            .map(str::trim)
            .map(|part| {
                question
                    .options
                    .iter()
                    .find(|option| option.key.eq_ignore_ascii_case(part))
                    .map(|option| option.key.clone())
                    .or_else(|| {
                        part.parse::<usize>().ok().and_then(|idx| {
                            idx.checked_sub(1)
                                .and_then(|zero_idx| question.options.get(zero_idx))
                                .map(|option| option.key.clone())
                        })
                    })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(selected) = selected.filter(|keys| !keys.is_empty()) {
            return ParsedPromptAnswer::Answer(Answer::multi_selected(selected));
        }
        return ParsedPromptAnswer::Invalid(invalid_choice_message(question));
    }
    if let Some(answer) = find_matching_option(&response, &question.options) {
        return ParsedPromptAnswer::Answer(answer);
    }
    if question.allow_freeform {
        return ParsedPromptAnswer::Answer(Answer::text(response));
    }
    ParsedPromptAnswer::Invalid(invalid_choice_message(question))
}

fn parse_confirm_response(prompt_read: PromptRead) -> ParsedPromptAnswer {
    let PromptRead::Line(response) = prompt_read else {
        return ParsedPromptAnswer::Interrupted;
    };
    match response.trim().to_lowercase().as_str() {
        "y" | "yes" => ParsedPromptAnswer::Answer(Answer::yes()),
        "n" | "no" => ParsedPromptAnswer::Answer(Answer::no()),
        _ => ParsedPromptAnswer::Invalid("Please enter y or n.".to_string()),
    }
}

fn parse_freeform_response(prompt_read: PromptRead) -> ParsedPromptAnswer {
    let PromptRead::Line(response) = prompt_read else {
        return ParsedPromptAnswer::Interrupted;
    };
    if response.trim().is_empty() {
        ParsedPromptAnswer::Invalid("Please enter a response.".to_string())
    } else {
        ParsedPromptAnswer::Answer(Answer::text(response))
    }
}

fn invalid_choice_message(question: &Question) -> String {
    let keys = question
        .options
        .iter()
        .map(|option| option.key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if keys.is_empty() {
        return "Please enter one of the listed options.".to_string();
    }
    format!("Please enter one of: {keys}.")
}

fn find_matching_option(response: &str, options: &[InterviewOption]) -> Option<Answer> {
    let trimmed = response.trim();
    for opt in options {
        if opt.key.eq_ignore_ascii_case(trimmed) {
            return Some(Answer {
                value:           AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text:            None,
            });
        }
    }
    if let Ok(idx) = trimmed.parse::<usize>() {
        if idx >= 1 && idx <= options.len() {
            let opt = &options[idx - 1];
            return Some(Answer {
                value:           AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text:            None,
            });
        }
    }
    None
}

async fn submit_server_interview_answer(
    client: &server_client::Client,
    run_id: &RunId,
    qid: &str,
    answer: &fabro_interview::Answer,
) -> Result<bool> {
    let body = match &answer.value {
        AnswerValue::Text(text) => types::SubmitAnswerTextRequest {
            kind: types::SubmitAnswerTextRequestKind::Text,
            text: text.clone(),
        }
        .into(),
        AnswerValue::Selected(key) => types::SubmitAnswerSelectedRequest {
            kind:       types::SubmitAnswerSelectedRequestKind::Selected,
            option_key: key.clone(),
        }
        .into(),
        AnswerValue::MultiSelected(keys) => types::SubmitAnswerMultiSelectedRequest {
            kind:        types::SubmitAnswerMultiSelectedRequestKind::MultiSelected,
            option_keys: keys.clone(),
        }
        .into(),
        AnswerValue::Yes => types::SubmitAnswerYesRequest {
            kind: types::SubmitAnswerYesRequestKind::Yes,
        }
        .into(),
        AnswerValue::No => types::SubmitAnswerNoRequest {
            kind: types::SubmitAnswerNoRequestKind::No,
        }
        .into(),
        AnswerValue::Cancelled
        | AnswerValue::Interrupted
        | AnswerValue::Skipped
        | AnswerValue::Timeout => {
            return Ok(false);
        }
    };
    client.submit_run_answer(run_id, qid, body).await?;
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
            .is_some_and(|status| status.is_terminal())
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
    let mut value = normalize_json_value(event.event.to_value()?);
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
            StageOutcome::Succeeded | StageOutcome::PartiallySucceeded
        );
        return Some(if success {
            ExitCode::from(0)
        } else {
            ExitCode::from(1)
        });
    }

    match state.status {
        Some(RunStatus::Succeeded { .. }) => Some(ExitCode::from(0)),
        Some(status) if status.is_terminal() => Some(ExitCode::from(1)),
        Some(_) | None => None,
    }
}

fn event_exit_code(event: &EventEnvelope) -> Option<ExitCode> {
    match &event.event.body {
        EventBody::RunCompleted(props) => Some(
            if props.status == "succeeded" || props.status == "partially_succeeded" {
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
    matches!(event.event.body, EventBody::InterviewStarted(_))
}

fn event_resolves_interview(event: &EventEnvelope, question_id: &str) -> bool {
    match &event.event.body {
        EventBody::InterviewCompleted(props) => props.question_id == question_id,
        EventBody::InterviewInterrupted(props) => props.question_id == question_id,
        EventBody::InterviewTimeout(props) => props.question_id == question_id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::absolute_paths,
        reason = "This test module prefers explicit type paths over extra imports."
    )]

    use fabro_interview::{Answer, AnswerValue};
    use fabro_util::terminal::Styles;
    use httpmock::MockServer;

    use super::*;

    fn no_color_styles() -> &'static Styles {
        Box::leak(Box::new(Styles::new(false)))
    }

    fn terminal_run_state_response() -> serde_json::Value {
        serde_json::json!({
            "spec": null,
            "graph_source": null,
            "start": null,
            "status": {
                "kind": "failed",
                "reason": "cancelled"
            },
            "status_updated_at": "2026-04-05T12:00:02Z",
            "checkpoint": null,
            "checkpoints": [],
            "conclusion": null,
            "sandbox": null,
            "final_patch": null,
            "pull_request": null,
            "stages": {}
        })
    }

    fn cancel_run_response(run_id: RunId) -> serde_json::Value {
        serde_json::json!({
            "id": run_id,
            "status": {
                "kind": "failed",
                "reason": "cancelled"
            },
            "error": null,
            "queue_position": null,
            "pending_control": null,
            "created_at": "2026-04-05T12:00:00Z"
        })
    }

    #[tokio::test]
    async fn attach_errors_without_store_context() {
        let dir = tempfile::tempdir().unwrap();

        let err = Box::pin(attach_run(
            dir.path(),
            None,
            None,
            false,
            no_color_styles(),
            false,
            false,
        ))
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
            value:           AnswerValue::Interrupted,
            selected_option: None,
            text:            None,
        };
        let skipped = Answer {
            value:           AnswerValue::Skipped,
            selected_option: None,
            text:            None,
        };
        let answered = Answer::yes();

        assert!(answer_requires_reattach(&interrupted));
        assert!(answer_requires_reattach(&skipped));
        assert!(!answer_requires_reattach(&answered));
    }

    #[test]
    fn invalid_confirm_response_is_user_correctable() {
        let response = parse_confirm_response(PromptRead::Line("dasf".to_string()));

        assert!(matches!(response, ParsedPromptAnswer::Invalid(_)));
    }

    #[test]
    fn eof_confirm_response_is_interrupted() {
        let response = parse_confirm_response(PromptRead::Eof);

        assert!(matches!(response, ParsedPromptAnswer::Interrupted));
    }

    #[test]
    fn invalid_multiple_choice_without_freeform_is_user_correctable() {
        let mut question = Question::new("Pick one.", QuestionType::MultipleChoice);
        question.options = vec![InterviewOption {
            key:   "A".to_string(),
            label: "Approve".to_string(),
        }];

        let response = parse_choice_response(&question, PromptRead::Line("bogus".to_string()));

        assert!(matches!(response, ParsedPromptAnswer::Invalid(_)));
    }

    #[test]
    fn unmatched_multiple_choice_with_freeform_remains_text() {
        let mut question = Question::new("Pick one.", QuestionType::MultipleChoice);
        question.options = vec![InterviewOption {
            key:   "A".to_string(),
            label: "Approve".to_string(),
        }];
        question.allow_freeform = true;

        let response = parse_choice_response(&question, PromptRead::Line("custom".to_string()));

        assert!(matches!(
            response,
            ParsedPromptAnswer::Answer(Answer {
                value: AnswerValue::Text(text),
                ..
            }) if text == "custom"
        ));
    }

    #[test]
    fn invalid_multi_select_token_is_user_correctable() {
        let mut question = Question::new("Pick many.", QuestionType::MultiSelect);
        question.options = vec![
            InterviewOption {
                key:   "A".to_string(),
                label: "Approve".to_string(),
            },
            InterviewOption {
                key:   "N".to_string(),
                label: "Notify".to_string(),
            },
        ];

        let response = parse_choice_response(&question, PromptRead::Line("A bogus".to_string()));

        assert!(matches!(response, ParsedPromptAnswer::Invalid(_)));
    }

    #[test]
    fn empty_freeform_response_is_user_correctable() {
        let response = parse_freeform_response(PromptRead::Line("   ".to_string()));

        assert!(matches!(response, ParsedPromptAnswer::Invalid(_)));
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
        let client = server_client::Client::new_no_proxy(&server.base_url()).unwrap();

        handle_detach_signal(&client, &run_id, true, Printer::Default).await;

        cancel_mock.assert();
        state_mock.assert();
    }
}
