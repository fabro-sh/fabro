use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use fabro_interview::{AnswerValue, ConsoleInterviewer};
use fabro_store::RuntimeState;
use fabro_util::terminal::Styles;
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::records::{Conclusion, ConclusionExt, RunRecord, RunRecordExt};
use fabro_workflows::run_status::{RunStatus, RunStatusRecord, RunStatusRecordExt};
use tokio::signal::ctrl_c;
use tokio::time::sleep;

use super::run_progress;

#[cfg(test)]
const ATTACH_STARTUP_GRACE: Duration = Duration::from_millis(200);
#[cfg(not(test))]
const ATTACH_STARTUP_GRACE: Duration = Duration::from_secs(3);
const INTERVIEW_UNANSWERED_MESSAGE: &str =
    "Interview ended without an answer. The run is still waiting for input; reattach to answer it.";

/// Attach to a running (or finished) workflow run, rendering progress live.
///
/// Returns exit code 0 for success/partial_success, 1 otherwise.
pub(crate) async fn attach_run(
    run_dir: &Path,
    kill_on_detach: bool,
    styles: &'static Styles,
    engine_child: Option<std::process::Child>,
) -> Result<ExitCode> {
    let progress_path = run_dir.join("progress.jsonl");
    let conclusion_path = run_dir.join("conclusion.json");
    let status_path = run_dir.join("status.json");
    let runtime_state = RuntimeState::new(run_dir);
    let runtime_interview_paths = InterviewPaths::from_runtime_state(&runtime_state);
    let legacy_interview_paths = InterviewPaths::from_base_dir(run_dir);

    let mut engine_guard = engine_child.map(EngineChildGuard::new);

    let is_tty = std::io::stderr().is_terminal();
    let verbose = RunRecord::load(run_dir)
        .map(|record| record.settings.verbose_enabled())
        .unwrap_or(false);
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

    // Wait for progress.jsonl to appear.
    // If the engine dies during early init (before any event is emitted),
    // progress.jsonl may never be created. Check for terminal status or
    // engine death so we surface the real failure instead of timing out.
    let mut wait_count = 0;
    while !progress_path.exists() {
        sleep(std::time::Duration::from_millis(100)).await;
        wait_count += 1;

        // Check if engine died before writing any progress
        if let Some(record) = read_status_record(&status_path) {
            if record.status.is_terminal() {
                progress_ui.finish();
                return Ok(determine_exit_code(&conclusion_path, Some(record)));
            }
        }

        if let Some(guard) = engine_guard.as_mut() {
            if let Some(child) = guard.inner() {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    // Engine exited without writing progress.jsonl
                    progress_ui.finish();
                    return Ok(determine_exit_code(
                        &conclusion_path,
                        read_status_record(&status_path),
                    ));
                }
            }
        }

        if let Some(pid) = read_launcher_pid(run_dir) {
            if !process_alive(pid) && wait_count > 5 {
                progress_ui.finish();
                return Ok(determine_exit_code(
                    &conclusion_path,
                    read_status_record(&status_path),
                ));
            }
        }

        if wait_count > 100 {
            // Guard's Drop kills+waits on the engine child
            drop(engine_guard.take());
            bail!(
                "Timed out waiting for progress.jsonl to appear in {}",
                run_dir.display()
            );
        }
        if cancelled.load(Ordering::Relaxed) {
            if !kill_on_detach {
                if let Some(guard) = engine_guard.as_mut() {
                    guard.defuse();
                }
            }
            // Guard's Drop kills+waits when kill_on_detach is true
            return Ok(ExitCode::from(1));
        }
    }

    let file = std::fs::File::open(&progress_path)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
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
                    if conclusion_path.exists()
                        || read_status_record(&status_path)
                            .is_some_and(|record| record.status.is_terminal())
                    {
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

        // Read new lines from progress.jsonl
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line)?;
            if bytes_read == 0 {
                break;
            }
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                progress_ui.handle_json_line(trimmed);
            }
        }

        // Check for interview request
        if let Some(interview_paths) =
            active_interview_paths(&runtime_interview_paths, &legacy_interview_paths)
        {
            if !interview_paths.response_path.exists() {
                if let Some(_claim_guard) = InterviewClaimGuard::acquire(&interview_paths.base_dir)
                {
                    if let Ok(request_data) = std::fs::read_to_string(&interview_paths.request_path)
                    {
                        if let Ok(question) =
                            serde_json::from_str::<fabro_interview::Question>(&request_data)
                        {
                            // Hide progress bars during interview
                            progress_ui.hide_bars();

                            // Prompt user via ConsoleInterviewer
                            let interviewer = ConsoleInterviewer::new(styles);
                            let answer =
                                fabro_interview::Interviewer::ask(&interviewer, question).await;

                            // Show progress bars again before any return path.
                            progress_ui.show_bars();

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

        let terminal_status = read_status_record(&status_path)
            .map(|record| record.status)
            .filter(|status| status.is_terminal());

        let child_alive_via_handle = engine_guard.as_mut().and_then(|guard| {
            guard.inner().map(|child| match child.try_wait() {
                Ok(None) => true,              // still running
                Ok(Some(_)) | Err(_) => false, // exited or error
            })
        });

        if let Some(child_alive) = child_alive_via_handle {
            if !child_alive {
                drain_remaining(&mut reader, &mut line, &mut progress_ui);
                break;
            }
        } else {
            if terminal_status.is_some() {
                drain_remaining(&mut reader, &mut line, &mut progress_ui);
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
                            || !progress_file_is_empty(&progress_path)
                    }
                }
            };
            if !engine_alive {
                drain_remaining(&mut reader, &mut line, &mut progress_ui);
                break;
            }
        }

        sleep(Duration::from_millis(100)).await;
    }

    // Finish progress bars
    progress_ui.finish();

    Ok(determine_exit_code(
        &conclusion_path,
        read_status_record(&status_path),
    ))
}

fn drain_remaining(
    reader: &mut BufReader<std::fs::File>,
    line: &mut String,
    progress_ui: &mut run_progress::ProgressUI,
) {
    loop {
        line.clear();
        match reader.read_line(line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    progress_ui.handle_json_line(trimmed);
                }
            }
        }
    }
}

fn read_status_record(path: &Path) -> Option<RunStatusRecord> {
    RunStatusRecord::load(path).ok()
}

fn read_launcher_pid(run_dir: &Path) -> Option<u32> {
    super::launcher::active_launcher_record_for_run(run_dir)
        .map(|record| record.pid)
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("run.pid"))
                .ok()
                .and_then(|pid| pid.trim().parse::<u32>().ok())
        })
}

fn progress_file_is_empty(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.len() == 0)
        .unwrap_or(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InterviewPaths {
    base_dir: PathBuf,
    request_path: PathBuf,
    response_path: PathBuf,
}

impl InterviewPaths {
    fn from_base_dir(base_dir: &Path) -> Self {
        let base_dir = base_dir.to_path_buf();
        Self {
            request_path: base_dir.join("interview_request.json"),
            response_path: base_dir.join("interview_response.json"),
            base_dir,
        }
    }

    fn from_runtime_state(runtime_state: &RuntimeState) -> Self {
        Self {
            base_dir: runtime_state.runtime_dir(),
            request_path: runtime_state.interview_request_path(),
            response_path: runtime_state.interview_response_path(),
        }
    }
}

fn active_interview_paths(
    runtime_paths: &InterviewPaths,
    legacy_paths: &InterviewPaths,
) -> Option<InterviewPaths> {
    if runtime_paths.request_path.exists() {
        Some(runtime_paths.clone())
    } else if legacy_paths.request_path.exists() {
        Some(legacy_paths.clone())
    } else {
        None
    }
}

fn interview_claim_path(base_dir: &Path) -> PathBuf {
    base_dir.join("interview_request.claim")
}

struct InterviewClaimGuard {
    claim_path: PathBuf,
}

impl InterviewClaimGuard {
    fn acquire(base_dir: &Path) -> Option<Self> {
        if try_claim_interview_request(base_dir) {
            Some(Self {
                claim_path: interview_claim_path(base_dir),
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

impl Drop for EngineChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn try_claim_interview_request(base_dir: &Path) -> bool {
    if std::fs::create_dir_all(base_dir).is_err() {
        return false;
    }

    let claim_path = interview_claim_path(base_dir);
    if let Ok(existing) = std::fs::read_to_string(&claim_path) {
        if let Ok(pid) = existing.trim().parse::<u32>() {
            if process_alive(pid) {
                return pid == std::process::id();
            }
        }
        let _ = std::fs::remove_file(&claim_path);
    }

    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&claim_path)
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

fn determine_exit_code(conclusion_path: &Path, status_record: Option<RunStatusRecord>) -> ExitCode {
    if conclusion_path.exists() {
        if let Ok(conclusion) = Conclusion::load(conclusion_path) {
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
    }

    match status_record.map(|record| record.status) {
        Some(RunStatus::Succeeded) => ExitCode::from(0),
        Some(_) | None => ExitCode::from(1),
    }
}

#[allow(unsafe_code)]
fn kill_engine(run_dir: &Path) {
    if let Some(pid) = read_launcher_pid(run_dir).map(|pid| i32::try_from(pid).unwrap()) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let _ = pid;
    }
}

#[allow(unsafe_code)]
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use fabro_interview::{Answer, AnswerValue};
    use fabro_util::terminal::Styles;
    use fabro_workflows::outcome::StageStatus;
    use fabro_workflows::records::Conclusion;
    use fabro_workflows::run_status::{StatusReason, write_run_status};

    fn no_color_styles() -> &'static Styles {
        Box::leak(Box::new(Styles::new(false)))
    }

    fn sample_conclusion(status: StageStatus) -> Conclusion {
        Conclusion {
            timestamp: Utc::now(),
            status,
            duration_ms: 0,
            failure_reason: None,
            final_git_commit_sha: None,
            stages: Vec::new(),
            total_cost: None,
            total_retries: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            has_pricing: false,
        }
    }

    #[tokio::test]
    async fn attach_does_not_return_when_only_conclusion_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("progress.jsonl"), "").unwrap();
        sample_conclusion(StageStatus::Success)
            .save(&dir.path().join("conclusion.json"))
            .unwrap();

        let child = std::process::Command::new("sh")
            .args(["-c", "sleep 0.35"])
            .spawn()
            .unwrap();
        let started = Instant::now();

        let exit = attach_run(dir.path(), false, no_color_styles(), Some(child))
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::from(0));
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "attach returned before the owned child exited"
        );
    }

    #[tokio::test]
    async fn attach_missing_pid_and_failed_status_is_not_alive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("progress.jsonl"), "").unwrap();
        write_run_status(
            dir.path(),
            RunStatus::Failed,
            Some(StatusReason::LaunchFailed),
        );

        let exit = attach_run(dir.path(), false, no_color_styles(), None)
            .await
            .unwrap();

        assert_eq!(exit, ExitCode::from(1));
    }

    #[test]
    fn try_claim_interview_request_reclaims_stale_claim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(interview_claim_path(dir.path()), "999999\n").unwrap();

        assert!(try_claim_interview_request(dir.path()));
        assert_eq!(
            std::fs::read_to_string(interview_claim_path(dir.path())).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn interview_claim_guard_releases_claim_on_drop() {
        let dir = tempfile::tempdir().unwrap();

        {
            let _guard = InterviewClaimGuard::acquire(dir.path()).unwrap();
            assert!(interview_claim_path(dir.path()).exists());
        }

        assert!(!interview_claim_path(dir.path()).exists());
    }

    #[test]
    fn active_interview_paths_prefers_runtime_paths() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_state = RuntimeState::new(dir.path());
        let runtime_paths = InterviewPaths::from_runtime_state(&runtime_state);
        let legacy_paths = InterviewPaths::from_base_dir(dir.path());

        std::fs::create_dir_all(runtime_state.runtime_dir()).unwrap();
        std::fs::write(&runtime_paths.request_path, "{}").unwrap();
        std::fs::write(&legacy_paths.request_path, "{}").unwrap();

        assert_eq!(
            active_interview_paths(&runtime_paths, &legacy_paths),
            Some(runtime_paths)
        );
    }

    #[test]
    fn active_interview_paths_falls_back_to_legacy_paths() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_state = RuntimeState::new(dir.path());
        let runtime_paths = InterviewPaths::from_runtime_state(&runtime_state);
        let legacy_paths = InterviewPaths::from_base_dir(dir.path());

        std::fs::write(&legacy_paths.request_path, "{}").unwrap();

        assert_eq!(
            active_interview_paths(&runtime_paths, &legacy_paths),
            Some(legacy_paths)
        );
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
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
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
