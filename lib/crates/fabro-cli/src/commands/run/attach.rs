use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use fabro_types::RunId;
use futures::StreamExt;

use fabro_interview::{AnswerValue, ConsoleInterviewer};
use fabro_store::{EventEnvelope, RuntimeState, SlateRunStore};
use fabro_util::json::normalize_json_value;
use fabro_util::terminal::Styles;
use fabro_workflow::outcome::StageStatus;
use fabro_workflow::run_status::RunStatus;
use tokio::signal::ctrl_c;
use tokio::time::{self, sleep};

use super::run_progress;
use crate::store;

#[cfg(test)]
const ATTACH_STARTUP_GRACE: Duration = Duration::from_millis(200);
#[cfg(not(test))]
const ATTACH_STARTUP_GRACE: Duration = Duration::from_secs(3);
const INTERVIEW_UNANSWERED_MESSAGE: &str =
    "Interview ended without an answer. The run is still waiting for input; reattach to answer it.";
const JSON_INTERVIEW_MESSAGE: &str = "This run is waiting for human input, but --json is non-interactive. Reattach without --json to answer it.";
const ATTACH_FINAL_STATUS_GRACE: Duration = Duration::from_secs(2);

/// Attach to a running (or finished) workflow run, rendering progress live.
///
/// Returns exit code 0 for success/partial_success, 1 otherwise.
pub(crate) async fn attach_run(
    run_dir: &Path,
    storage_dir: Option<&Path>,
    run_id: Option<&RunId>,
    kill_on_detach: bool,
    styles: &'static Styles,
    engine_child: Option<std::process::Child>,
    json_output: bool,
) -> Result<ExitCode> {
    let inferred_storage_dir = infer_storage_dir(run_dir);
    let inferred_run_id = infer_run_id(run_dir);
    let storage_dir = storage_dir.map(Path::to_path_buf).or(inferred_storage_dir);
    let run_id = run_id.copied().or(inferred_run_id);

    if let (Some(storage_dir), Some(run_id)) = (storage_dir.as_deref(), run_id.as_ref()) {
        match store::open_run_reader(storage_dir, run_id).await {
            Ok(run_store) => match run_store.list_events().await {
                Ok(events) => {
                    let verbose = run_store
                        .state()
                        .await
                        .ok()
                        .and_then(|state| state.run)
                        .is_some_and(|record| record.settings.verbose_enabled());
                    let event_lines = events
                        .iter()
                        .map(event_payload_line)
                        .collect::<Result<Vec<_>>>()?;
                    return attach_run_store(
                        run_dir,
                        &run_store,
                        verbose,
                        event_lines,
                        events.last().map_or(0, |event| event.seq),
                        kill_on_detach,
                        styles,
                        engine_child,
                        json_output,
                    )
                    .await;
                }
                Err(err) => {
                    return Err(anyhow::anyhow!(
                        "Failed to list events from SlateDB for run {run_id}: {err}"
                    ));
                }
            },
            Err(err) => {
                return Err(anyhow::anyhow!(
                    "Failed to open SlateDB reader for run {run_id}: {err}"
                ));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Could not infer SlateDB storage location and run id for attach"
    ))
}

async fn attach_run_store(
    run_dir: &Path,
    run_store: &SlateRunStore,
    verbose: bool,
    existing_events: Vec<String>,
    last_seq: u32,
    kill_on_detach: bool,
    styles: &'static Styles,
    engine_child: Option<std::process::Child>,
    json_output: bool,
) -> Result<ExitCode> {
    let runtime_state = RuntimeState::new(run_dir);
    let runtime_interview_paths = InterviewPaths::from_runtime_state(&runtime_state);

    let mut engine_guard = engine_child.map(EngineChildGuard::new);

    let is_tty = std::io::stderr().is_terminal();
    let mut progress_ui = run_progress::ProgressUI::new(is_tty, verbose);

    // Install Ctrl+C handler
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        tokio::spawn(async move {
            let _ = ctrl_c().await;
            cancelled.store(true, Ordering::Relaxed);
        });
    }

    for line in &existing_events {
        emit_progress_line(&mut progress_ui, line, json_output)?;
    }

    let mut stream = run_store.watch_events_from(if last_seq == 0 { 1 } else { last_seq + 1 })?;
    let mut next_seq = if last_seq == 0 { 1 } else { last_seq + 1 };
    let mut cached_pid: Option<u32> = None;
    let attach_started = Instant::now();

    loop {
        if cancelled.load(Ordering::Relaxed) {
            if kill_on_detach {
                if let Some(guard) = engine_guard.as_mut() {
                    if let Some(child) = guard.inner() {
                        let _ = child.kill();
                    }
                } else {
                    kill_engine(run_dir);
                }
                // Wait briefly for a terminal status or conclusion
                for _ in 0..20 {
                    if run_store.state().await.ok().is_some_and(|state| {
                        state.conclusion.is_some()
                            || state
                                .status
                                .is_some_and(|record| record.status.is_terminal())
                    }) {
                        break;
                    }
                    sleep(Duration::from_millis(100)).await;
                }
            } else {
                if let Some(guard) = engine_guard.as_mut() {
                    guard.defuse();
                }
                eprintln!("Detached from run (engine continues in background)");
            }
            break;
        }

        let mut saw_event = false;
        match time::timeout(Duration::from_millis(100), stream.next()).await {
            Ok(Some(Ok(event))) => {
                let line = event_payload_line(&event)?;
                emit_progress_line(&mut progress_ui, &line, json_output)?;
                next_seq = event.seq.saturating_add(1);
                saw_event = true;
            }
            Ok(Some(Err(err))) => return Err(err.into()),
            Ok(None) | Err(_) => {}
        }

        // Check for interview request
        if runtime_interview_paths.request_path.exists() {
            let interview_paths = &runtime_interview_paths;
            if !interview_paths.response_path.exists() {
                if json_output {
                    defuse_engine_child(&mut engine_guard);
                    eprintln!("{JSON_INTERVIEW_MESSAGE}");
                    return Ok(ExitCode::from(1));
                }
                if let Some(_claim_guard) =
                    InterviewClaimGuard::acquire(&interview_paths.claim_path)
                {
                    if let Ok(request_data) = std::fs::read_to_string(&interview_paths.request_path)
                    {
                        if let Ok(question) =
                            serde_json::from_str::<fabro_interview::Question>(&request_data)
                        {
                            // Hide progress bars during interview
                            hide_progress(&mut progress_ui, json_output);

                            // Prompt user via ConsoleInterviewer
                            let interviewer = ConsoleInterviewer::new(styles);
                            let answer =
                                fabro_interview::Interviewer::ask(&interviewer, question).await;

                            // Show progress bars again before any return path.
                            show_progress(&mut progress_ui, json_output);

                            if answer_requires_reattach(&answer) {
                                if let Some(guard) = engine_guard.as_mut() {
                                    guard.defuse();
                                }
                                eprintln!("{INTERVIEW_UNANSWERED_MESSAGE}");
                                return Ok(ExitCode::from(1));
                            }

                            write_interview_response_atomically(
                                &interview_paths.response_path,
                                &answer,
                            )?;
                        }
                    }
                }
            }
        }

        let terminal_status = run_store
            .state()
            .await
            .ok()
            .and_then(|state| state.status.map(|record| record.status))
            .filter(|status| status.is_terminal());

        let child_alive_via_handle = engine_guard.as_mut().and_then(|guard| {
            guard.inner().map(|child| match child.try_wait() {
                Ok(None) => true,              // still running
                Ok(Some(_)) | Err(_) => false, // exited or error
            })
        });

        if let Some(child_alive) = child_alive_via_handle {
            if !child_alive && !saw_event {
                flush_remaining_store_events(run_store, next_seq, &mut progress_ui, json_output)
                    .await?;
                break;
            }
        } else {
            if terminal_status.is_some() && !saw_event {
                flush_remaining_store_events(run_store, next_seq, &mut progress_ui, json_output)
                    .await?;
                break;
            }

            let engine_alive = match cached_pid {
                Some(pid) => process_alive(pid),
                None => {
                    if let Some(pid) = read_launcher_pid(run_dir) {
                        cached_pid = Some(pid);
                        process_alive(pid)
                    } else {
                        attach_started.elapsed() < ATTACH_STARTUP_GRACE
                    }
                }
            };
            if !engine_alive {
                flush_remaining_store_events(run_store, next_seq, &mut progress_ui, json_output)
                    .await?;
                break;
            }
        }
    }

    finish_progress(&mut progress_ui, json_output);

    Ok(determine_exit_code_with_store(run_store).await)
}

async fn flush_remaining_store_events(
    run_store: &SlateRunStore,
    mut next_seq: u32,
    progress_ui: &mut run_progress::ProgressUI,
    json_output: bool,
) -> Result<()> {
    let deadline = Instant::now() + ATTACH_FINAL_STATUS_GRACE;
    loop {
        let mut saw_new_event = false;
        let events = run_store
            .list_events()
            .await?
            .into_iter()
            .filter(|e| e.seq >= next_seq)
            .collect::<Vec<_>>();
        for event in events {
            let line = event_payload_line(&event)?;
            emit_progress_line(progress_ui, &line, json_output)?;
            next_seq = event.seq.saturating_add(1);
            saw_new_event = true;
        }

        if Instant::now() >= deadline {
            break;
        }

        if !saw_new_event {
            sleep(Duration::from_millis(100)).await;
        }
    }

    Ok(())
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
    serde_json::to_string(&normalize_json_value(event.payload.as_value().clone()))
        .map_err(Into::into)
}

fn read_launcher_pid(run_dir: &Path) -> Option<u32> {
    super::launcher::active_launcher_record_for_run(run_dir).map(|record| record.pid)
}

fn infer_storage_dir(run_dir: &Path) -> Option<PathBuf> {
    let runs_dir = run_dir.parent()?;
    let storage_dir = runs_dir.parent()?;
    (runs_dir.file_name()? == "runs").then(|| storage_dir.to_path_buf())
}

fn infer_run_id(run_dir: &Path) -> Option<RunId> {
    super::launcher::launcher_record_for_run(run_dir)
        .map(|record| record.run_id)
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("id.txt"))
                .ok()
                .map(|run_id| run_id.trim().to_string())
                .filter(|run_id| !run_id.is_empty())
                .and_then(|run_id| run_id.parse().ok())
        })
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct InterviewPaths {
    claim_path: PathBuf,
    request_path: PathBuf,
    response_path: PathBuf,
}

impl InterviewPaths {
    fn from_runtime_state(runtime_state: &RuntimeState) -> Self {
        Self {
            claim_path: runtime_state.interview_claim_path(),
            request_path: runtime_state.interview_request_path(),
            response_path: runtime_state.interview_response_path(),
        }
    }
}

struct InterviewClaimGuard {
    claim_path: PathBuf,
}

impl InterviewClaimGuard {
    fn acquire(claim_path: &Path) -> Option<Self> {
        if try_claim_interview_request(claim_path) {
            Some(Self {
                claim_path: claim_path.to_path_buf(),
            })
        } else {
            None
        }
    }
}

impl Drop for InterviewClaimGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.claim_path);
    }
}

struct EngineChildGuard {
    child: Option<std::process::Child>,
}

impl EngineChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self { child: Some(child) }
    }

    fn inner(&mut self) -> Option<&mut std::process::Child> {
        self.child.as_mut()
    }

    fn defuse(&mut self) {
        self.child.take();
    }
}

fn defuse_engine_child(engine_guard: &mut Option<EngineChildGuard>) {
    if let Some(guard) = engine_guard.as_mut() {
        guard.defuse();
    }
}

impl Drop for EngineChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn try_claim_interview_request(claim_path: &Path) -> bool {
    if let Some(parent) = claim_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }

    if let Ok(existing) = std::fs::read_to_string(claim_path) {
        if let Ok(pid) = existing.trim().parse::<u32>() {
            if process_alive(pid) {
                return pid == std::process::id();
            }
        }
        let _ = std::fs::remove_file(claim_path);
    }

    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(claim_path)
    {
        Ok(mut file) => {
            let _ = writeln!(file, "{}", std::process::id());
            true
        }
        Err(_) => false,
    }
}

fn answer_requires_reattach(answer: &fabro_interview::Answer) -> bool {
    matches!(answer.value, AnswerValue::Aborted | AnswerValue::Skipped)
}

fn write_interview_response_atomically(
    response_path: &Path,
    answer: &fabro_interview::Answer,
) -> Result<()> {
    let response_json = serde_json::to_string_pretty(answer)?;
    if let Some(parent) = response_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_path = response_path.with_extension("json.tmp");
    std::fs::write(&temp_path, response_json)?;
    std::fs::rename(temp_path, response_path)?;
    Ok(())
}

async fn determine_exit_code_with_store(run_store: &SlateRunStore) -> ExitCode {
    let deadline = Instant::now() + ATTACH_FINAL_STATUS_GRACE;
    loop {
        if let Ok(state) = run_store.state().await {
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

fn kill_engine(run_dir: &Path) {
    if let Some(pid) = read_launcher_pid(run_dir) {
        fabro_proc::sigterm(pid);
    }
}

fn process_alive(pid: u32) -> bool {
    fabro_proc::process_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::run::launcher;
    use chrono::Utc;
    use fabro_interview::{Answer, AnswerValue};
    use fabro_util::terminal::Styles;

    fn no_color_styles() -> &'static Styles {
        Box::leak(Box::new(Styles::new(false)))
    }

    #[tokio::test]
    async fn attach_errors_without_store_context() {
        let dir = tempfile::tempdir().unwrap();

        let err = attach_run(
            dir.path(),
            None,
            None,
            false,
            no_color_styles(),
            None,
            false,
        )
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
            .join("runs")
            .join("20260401-test");
        std::fs::create_dir_all(&run_dir).unwrap();

        assert_eq!(
            infer_storage_dir(&run_dir),
            Some(dir.path().join("storage"))
        );
    }

    #[test]
    fn infer_run_id_uses_launcher_record_without_run_json() {
        let dir = tempfile::tempdir().unwrap();
        let storage_dir = dir.path().join("storage");
        let run_dir = storage_dir.join("runs").join("20260401-test");
        std::fs::create_dir_all(&run_dir).unwrap();

        launcher::write_launcher_record(
            &launcher::launcher_record_path(&storage_dir, &fabro_types::fixtures::RUN_1),
            &launcher::LauncherRecord {
                run_id: fabro_types::fixtures::RUN_1,
                run_dir: run_dir.clone(),
                pid: u32::MAX,
                resume: false,
                log_path: dir.path().join("launcher.log"),
                started_at: Utc::now(),
            },
        )
        .unwrap();

        assert_eq!(infer_run_id(&run_dir), Some(fabro_types::fixtures::RUN_1));
    }

    #[test]
    fn try_claim_interview_request_reclaims_stale_claim() {
        let dir = tempfile::tempdir().unwrap();
        let claim_path = dir.path().join("runtime").join("interview_request.claim");
        std::fs::create_dir_all(claim_path.parent().unwrap()).unwrap();
        std::fs::write(&claim_path, "999999\n").unwrap();

        assert!(try_claim_interview_request(&claim_path));
        assert_eq!(
            std::fs::read_to_string(claim_path).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn interview_claim_guard_releases_claim_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let claim_path = dir.path().join("runtime").join("interview_request.claim");

        {
            let _guard = InterviewClaimGuard::acquire(&claim_path).unwrap();
            assert!(claim_path.exists());
        }

        assert!(!claim_path.exists());
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

    #[test]
    fn engine_child_guard_kills_on_drop() {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();

        {
            let _guard = EngineChildGuard::new(child);
        }

        // Process should be dead after guard is dropped
        assert!(
            !process_alive(pid),
            "process should be dead after guard drop"
        );
    }

    #[test]
    fn engine_child_guard_defuse_keeps_alive() {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();

        {
            let mut guard = EngineChildGuard::new(child);
            guard.defuse();
        }

        // Process should still be alive after defused guard is dropped
        assert!(
            process_alive(pid),
            "process should still be alive after defused guard drop"
        );

        // Clean up
        #[cfg(unix)]
        fabro_proc::sigkill(pid);
    }

    #[test]
    fn write_interview_response_atomically_persists_answer() {
        let dir = tempfile::tempdir().unwrap();
        let response_path = dir.path().join("interview_response.json");
        let answer = Answer {
            value: AnswerValue::Text("ship it".to_string()),
            selected_option: None,
            text: Some("ship it".to_string()),
        };

        write_interview_response_atomically(&response_path, &answer).unwrap();

        let saved: Answer =
            serde_json::from_str(&std::fs::read_to_string(&response_path).unwrap()).unwrap();
        assert_eq!(saved.text.as_deref(), Some("ship it"));
        assert!(!response_path.with_extension("json.tmp").exists());
    }
}
