use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use fabro_workflows::conclusion::Conclusion;
use fabro_workflows::event::{RunNoticeLevel, WorkflowRunEvent};
use fabro_workflows::outcome::StageStatus;
use fabro_workflows::run_status::{self, RunStatus, StatusReason};
use serde::Serialize;

const POSTRUN_ABORTED_MESSAGE: &str = "Run aborted before post-run finalization completed.";

pub(crate) struct DetachedRunBootstrapGuard {
    run_dir: PathBuf,
    active: bool,
}

impl DetachedRunBootstrapGuard {
    pub(crate) fn arm(run_dir: &Path) -> Result<Self> {
        std::fs::write(run_dir.join("run.pid"), std::process::id().to_string())
            .with_context(|| format!("Failed to write {}", run_dir.join("run.pid").display()))?;
        run_status::write_run_status(
            run_dir,
            RunStatus::Starting,
            Some(StatusReason::SandboxInitializing),
        );
        Ok(Self {
            run_dir: run_dir.to_path_buf(),
            active: true,
        })
    }

    pub(crate) fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for DetachedRunBootstrapGuard {
    fn drop(&mut self) {
        if self.active {
            run_status::write_run_status(
                &self.run_dir,
                RunStatus::Failed,
                Some(StatusReason::SandboxInitFailed),
            );
        }
    }
}

pub(crate) struct DetachedRunCompletionGuard {
    run_dir: PathBuf,
    active: bool,
}

impl DetachedRunCompletionGuard {
    pub(crate) fn arm(run_dir: &Path) -> Self {
        Self {
            run_dir: run_dir.to_path_buf(),
            active: true,
        }
    }

    pub(crate) fn defuse(&mut self) {
        self.active = false;
    }
}

impl Drop for DetachedRunCompletionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        run_status::write_run_status(
            &self.run_dir,
            RunStatus::Failed,
            Some(StatusReason::WorkflowError),
        );
        if !self.run_dir.join("conclusion.json").exists() {
            let _ = write_failure_conclusion(
                &self.run_dir,
                POSTRUN_ABORTED_MESSAGE,
                Some(StatusReason::WorkflowError),
            );
        }
        if let Some(run_id) = load_run_id(&self.run_dir) {
            let _ = append_progress_event(
                &self.run_dir,
                &run_id,
                &WorkflowRunEvent::RunNotice {
                    level: RunNoticeLevel::Error,
                    code: "postrun_aborted".to_string(),
                    message: POSTRUN_ABORTED_MESSAGE.to_string(),
                },
            );
        }
    }
}

pub(crate) fn load_run_id(run_dir: &Path) -> Option<String> {
    fabro_workflows::run_spec::RunSpec::load(run_dir)
        .ok()
        .map(|spec| spec.run_id)
        .filter(|run_id| !run_id.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("id.txt"))
                .ok()
                .map(|run_id| run_id.trim().to_string())
                .filter(|run_id| !run_id.is_empty())
        })
}

pub(crate) fn build_event_envelope(event: &WorkflowRunEvent, run_id: &str) -> serde_json::Value {
    let (event_name, event_fields) = fabro_workflows::event::flatten_event(event);
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "ts".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
    );
    envelope.insert(
        "run_id".to_string(),
        serde_json::Value::String(run_id.to_string()),
    );
    envelope.insert("event".to_string(), serde_json::Value::String(event_name));
    for (k, v) in event_fields {
        if k != "ts" && k != "run_id" && k != "event" {
            envelope.insert(k, v);
        }
    }
    serde_json::Value::Object(envelope)
}

pub(crate) fn append_progress_event(
    run_dir: &Path,
    run_id: &str,
    event: &WorkflowRunEvent,
) -> Result<()> {
    let envelope = build_event_envelope(event, run_id);
    let line = serde_json::to_string(&envelope)?;
    let line = fabro_util::redact::redact_jsonl_line(&line);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("progress.jsonl"))
        .with_context(|| {
            format!(
                "Failed to open {}",
                run_dir.join("progress.jsonl").display()
            )
        })?;
    writeln!(file, "{line}")?;

    let pretty = serde_json::to_string_pretty(&envelope)?;
    let pretty = fabro_util::redact::redact_jsonl_line(&pretty);
    std::fs::write(run_dir.join("live.json"), pretty)
        .with_context(|| format!("Failed to write {}", run_dir.join("live.json").display()))?;

    Ok(())
}

pub(crate) fn append_run_notice(
    run_dir: &Path,
    level: RunNoticeLevel,
    code: &'static str,
    message: impl Into<String>,
) -> Result<()> {
    let Some(run_id) = load_run_id(run_dir) else {
        return Ok(());
    };
    append_progress_event(
        run_dir,
        &run_id,
        &WorkflowRunEvent::RunNotice {
            level,
            code: code.to_string(),
            message: message.into(),
        },
    )
}

pub(crate) fn persist_detached_failure(
    run_dir: &Path,
    phase: &'static str,
    reason: StatusReason,
    error: &anyhow::Error,
) -> Result<()> {
    #[derive(Serialize)]
    struct DetachedFailureRecord<'a> {
        timestamp: chrono::DateTime<Utc>,
        phase: &'a str,
        reason: StatusReason,
        error: String,
    }

    let message = error.to_string();
    let record = DetachedFailureRecord {
        timestamp: Utc::now(),
        phase,
        reason,
        error: message.clone(),
    };
    std::fs::write(
        run_dir.join("detached_failure.json"),
        serde_json::to_string_pretty(&record)?,
    )
    .with_context(|| {
        format!(
            "Failed to write {}",
            run_dir.join("detached_failure.json").display()
        )
    })?;

    write_failure_conclusion(run_dir, &message, Some(reason))?;
    run_status::write_run_status(run_dir, RunStatus::Failed, Some(reason));

    if let Some(run_id) = load_run_id(run_dir) {
        append_progress_event(
            run_dir,
            &run_id,
            &WorkflowRunEvent::RunNotice {
                level: RunNoticeLevel::Error,
                code: format!("{phase}_failed"),
                message,
            },
        )?;
    }

    Ok(())
}

pub(crate) fn write_failure_conclusion(
    run_dir: &Path,
    message: &str,
    _reason: Option<StatusReason>,
) -> Result<()> {
    if run_dir.join("conclusion.json").exists() {
        return Ok(());
    }

    let conclusion = Conclusion {
        timestamp: Utc::now(),
        status: StageStatus::Fail,
        duration_ms: 0,
        failure_reason: Some(message.to_string()),
        final_git_commit_sha: None,
        stages: vec![],
        total_cost: None,
        total_retries: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        total_cache_write_tokens: 0,
        total_reasoning_tokens: 0,
        has_pricing: false,
    };
    conclusion.save(&run_dir.join("conclusion.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_workflows::run_status::{RunStatusRecord, StatusReason};

    #[test]
    fn bootstrap_guard_marks_failed_on_drop() {
        let dir = tempfile::tempdir().unwrap();

        {
            let _guard = DetachedRunBootstrapGuard::arm(dir.path()).unwrap();
            let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
            assert_eq!(record.status, RunStatus::Starting);
            assert_eq!(record.reason, Some(StatusReason::SandboxInitializing));
        }

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Failed);
        assert_eq!(record.reason, Some(StatusReason::SandboxInitFailed));
    }

    #[test]
    fn bootstrap_guard_defuse_leaves_starting_intact() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut guard = DetachedRunBootstrapGuard::arm(dir.path()).unwrap();
            guard.defuse();
        }

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Starting);
        assert_eq!(record.reason, Some(StatusReason::SandboxInitializing));
    }

    #[test]
    fn completion_guard_marks_failed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id.txt"), "run-123").unwrap();

        {
            let _guard = DetachedRunCompletionGuard::arm(dir.path());
        }

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Failed);
        assert_eq!(record.reason, Some(StatusReason::WorkflowError));
        assert!(dir.path().join("conclusion.json").exists());
        let progress = std::fs::read_to_string(dir.path().join("progress.jsonl")).unwrap();
        assert!(progress.contains("postrun_aborted"));
    }

    #[test]
    fn load_run_id_falls_back_to_id_txt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id.txt"), "run-xyz").unwrap();

        assert_eq!(load_run_id(dir.path()).as_deref(), Some("run-xyz"));
    }

    #[test]
    fn persist_detached_failure_writes_status_conclusion_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id.txt"), "run-err").unwrap();

        let err = anyhow::anyhow!("bootstrap exploded");
        persist_detached_failure(dir.path(), "bootstrap", StatusReason::BootstrapFailed, &err)
            .unwrap();

        let record = RunStatusRecord::load(&dir.path().join("status.json")).unwrap();
        assert_eq!(record.status, RunStatus::Failed);
        assert_eq!(record.reason, Some(StatusReason::BootstrapFailed));
        let conclusion =
            fabro_workflows::conclusion::Conclusion::load(&dir.path().join("conclusion.json"))
                .unwrap();
        assert_eq!(conclusion.status, StageStatus::Fail);
        assert_eq!(
            conclusion.failure_reason.as_deref(),
            Some("bootstrap exploded")
        );
        let progress = std::fs::read_to_string(dir.path().join("progress.jsonl")).unwrap();
        assert!(progress.contains("bootstrap_failed"));
        assert!(dir.path().join("detached_failure.json").exists());
    }

    #[test]
    fn append_run_notice_writes_progress() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id.txt"), "run-notice").unwrap();

        append_run_notice(
            dir.path(),
            RunNoticeLevel::Warn,
            "interview_unanswered",
            "The run is still waiting for input.",
        )
        .unwrap();

        let progress = std::fs::read_to_string(dir.path().join("progress.jsonl")).unwrap();
        assert!(progress.contains("\"event\":\"RunNotice\""));
        assert!(progress.contains("\"code\":\"interview_unanswered\""));
    }
}
