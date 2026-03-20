use std::io::{BufRead, BufReader, IsTerminal};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};

use fabro_interview::ConsoleInterviewer;
use fabro_util::terminal::Styles;

use super::run_progress;

/// Attach to a running (or finished) workflow run, rendering progress live.
///
/// Returns exit code 0 for success/partial_success, 1 otherwise.
pub async fn attach_run(
    run_dir: &Path,
    kill_on_detach: bool,
    styles: &'static Styles,
) -> Result<ExitCode> {
    let progress_path = run_dir.join("progress.jsonl");
    let conclusion_path = run_dir.join("conclusion.json");
    let interview_request_path = run_dir.join("interview_request.json");
    let interview_response_path = run_dir.join("interview_response.json");
    let pid_path = run_dir.join("run.pid");

    let is_tty = std::io::stderr().is_terminal();
    let progress_ui = Arc::new(Mutex::new(run_progress::ProgressUI::new(is_tty, false)));

    // Install Ctrl+C handler
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancelled.store(true, Ordering::Relaxed);
        });
    }

    // Wait for progress.jsonl to appear
    let mut wait_count = 0;
    while !progress_path.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        wait_count += 1;
        if wait_count > 100 {
            bail!(
                "Timed out waiting for progress.jsonl to appear in {}",
                run_dir.display()
            );
        }
        if cancelled.load(Ordering::Relaxed) {
            return Ok(ExitCode::from(0));
        }
    }

    let file = std::fs::File::open(&progress_path)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    loop {
        if cancelled.load(Ordering::Relaxed) {
            if kill_on_detach {
                // Kill the engine process
                kill_engine(&pid_path);
                // Wait briefly for conclusion
                for _ in 0..20 {
                    if conclusion_path.exists() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            } else {
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
                progress_ui
                    .lock()
                    .expect("progress lock poisoned")
                    .handle_json_line(trimmed);
            }
        }

        // Check for interview request
        if interview_request_path.exists() {
            if let Ok(request_data) = std::fs::read_to_string(&interview_request_path) {
                if let Ok(question) =
                    serde_json::from_str::<fabro_interview::Question>(&request_data)
                {
                    // Hide progress bars during interview
                    progress_ui
                        .lock()
                        .expect("progress lock poisoned")
                        .hide_bars();

                    // Prompt user via ConsoleInterviewer
                    let interviewer = ConsoleInterviewer::new(styles);
                    let answer = fabro_interview::Interviewer::ask(&interviewer, question).await;

                    // Write response
                    if let Ok(response_json) = serde_json::to_string_pretty(&answer) {
                        let _ = std::fs::write(&interview_response_path, response_json);
                    }

                    // Show progress bars again
                    progress_ui
                        .lock()
                        .expect("progress lock poisoned")
                        .show_bars();
                }
            }
        }

        // Check if run is complete
        if conclusion_path.exists() {
            // Drain any remaining lines
            drain_remaining(&mut reader, &mut line, &progress_ui);
            break;
        }

        // Check if engine process is still alive (if PID file exists)
        if pid_path.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                    if !process_alive(pid) && !conclusion_path.exists() {
                        // Engine died without writing conclusion — drain and exit
                        drain_remaining(&mut reader, &mut line, &progress_ui);
                        break;
                    }
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Finish progress bars
    progress_ui.lock().expect("progress lock poisoned").finish();

    // Determine exit code from conclusion
    if conclusion_path.exists() {
        match fabro_workflows::conclusion::Conclusion::load(&conclusion_path) {
            Ok(conclusion) => {
                let success = matches!(
                    conclusion.status,
                    fabro_workflows::outcome::StageStatus::Success
                        | fabro_workflows::outcome::StageStatus::PartialSuccess
                );
                Ok(if success {
                    ExitCode::from(0)
                } else {
                    ExitCode::from(1)
                })
            }
            Err(_) => Ok(ExitCode::from(1)),
        }
    } else {
        Ok(ExitCode::from(1))
    }
}

fn drain_remaining(
    reader: &mut BufReader<std::fs::File>,
    line: &mut String,
    progress_ui: &Arc<Mutex<run_progress::ProgressUI>>,
) {
    loop {
        line.clear();
        match reader.read_line(line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    progress_ui
                        .lock()
                        .expect("progress lock poisoned")
                        .handle_json_line(trimmed);
                }
            }
            Err(_) => break,
        }
    }
}

fn kill_engine(pid_path: &Path) {
    if let Ok(pid_str) = std::fs::read_to_string(pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            let _ = pid;
        }
    }
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
