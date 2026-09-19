//! Forking a run at a checkpoint: the Fabro run the fork becomes.
//!
//! A fork is a new run whose records Petri seeds from the source's up to a
//! checkpoint's position (`fabro_petri::fork`, over the timeline here). What
//! Fabro itself makes of it is a run row like any other: the `run.created`
//! record carrying the source's spec (its admission, settings and target)
//! under the new id, with `fork_source_ref` naming the source and the
//! checkpoint's commit, and `retried_from` when the fork is a retry; then
//! the `submitted` lifecycle transition. The run is then started in resume
//! mode, as a run left in flight is.

use std::path::PathBuf;

use fabro_store::Database;
use fabro_store::platform_records::{
    PlatformRecord, RunCreatedRecord, RunLifecycleKind, RunLifecycleRecord,
};
use fabro_types::{ForkSourceRef, RunId, RunProjection, RunProvenance, RunStatus};
use tokio::fs;

use super::ensure_not_archived;
use super::timeline::{TimelineEntry, TimelinePosition};
use crate::error::Error;

/// The checkpoint a fork was resolved to, as the API reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedForkTarget {
    pub checkpoint_ordinal: usize,
    pub node_id:            String,
    pub visit:              usize,
    pub position:           TimelinePosition,
    pub checkpoint_sha:     String,
}

impl ResolvedForkTarget {
    /// The entry as a fork target, refused when it has no commit.
    pub fn of(entry: &TimelineEntry) -> Result<Self, Error> {
        let checkpoint_sha = entry.run_commit_sha.clone().ok_or_else(|| {
            Error::Validation(format!(
                "checkpoint @{} has no git_commit_sha; cannot fork",
                entry.ordinal
            ))
        })?;
        Ok(Self {
            checkpoint_ordinal: entry.ordinal,
            node_id: entry.node_name.clone(),
            visit: usize::try_from(entry.visit).unwrap_or(1),
            position: entry.position,
            checkpoint_sha,
        })
    }

    #[must_use]
    pub fn response_target(&self) -> String {
        format!("@{}", self.checkpoint_ordinal)
    }
}

/// The new run a fork creates.
#[derive(Debug)]
pub struct ForkedRunInput<'a> {
    pub source:         &'a RunProjection,
    pub new_run_id:     RunId,
    /// The new run's scratch directory.
    pub run_dir:        PathBuf,
    pub checkpoint_sha: String,
    /// Who forked, when the fork records its own provenance; `None` keeps
    /// the source's.
    pub provenance:     Option<RunProvenance>,
    pub web_url:        Option<String>,
    /// The source, when the fork is a retry of it.
    pub retried_from:   Option<RunId>,
}

/// A run can be forked unless it is archived.
pub fn ensure_forkable(source: &RunProjection, run_id: &RunId) -> Result<(), Error> {
    ensure_not_archived(source.archived_at.is_some(), run_id)
}

/// A run must be terminal to be rewound or retried: its records are
/// complete, and nothing is writing them.
pub fn ensure_terminal(source: &RunProjection, run_id: &RunId, verb: &str) -> Result<(), Error> {
    let current = source.status;
    if current.is_terminal() {
        Ok(())
    } else {
        Err(Error::Precondition(format!(
            "run {run_id} must be terminal (succeeded, failed, or dead) to {verb}; current status \
             is {current}"
        )))
    }
}

/// The `run.created` record of the fork: the source's spec under the new
/// id, naming where it came from.
#[must_use]
pub fn forked_run_record(input: &ForkedRunInput<'_>) -> RunCreatedRecord {
    let mut spec = input.source.spec.clone();
    spec.run_id = input.new_run_id;
    spec.fork_source_ref = Some(ForkSourceRef {
        source_run_id:  input.source.spec.run_id,
        checkpoint_sha: input.checkpoint_sha.clone(),
    });
    if let Some(provenance) = &input.provenance {
        spec.provenance = provenance.clone();
    }
    RunCreatedRecord {
        spec,
        title: Some(input.source.title().into_owned()),
        parent_id: input.source.parent_id,
        retried_from: input.retried_from,
        web_url: input.web_url.clone(),
    }
}

/// Create the fork's run: its scratch directory, then its first records
/// (`run.created` and the `submitted` transition), which wake its
/// projector.
pub async fn persist_forked_run(store: &Database, input: &ForkedRunInput<'_>) -> Result<(), Error> {
    fs::create_dir_all(&input.run_dir).await.map_err(|err| {
        Error::Io(format!(
            "creating run directory {}: {err}",
            input.run_dir.display()
        ))
    })?;
    let created = PlatformRecord::RunCreated(forked_run_record(input));
    let submitted = PlatformRecord::RunLifecycle(
        RunLifecycleRecord::new(RunLifecycleKind::Submitted).with_status(RunStatus::Submitted),
    );
    let summaries = store.run_summary_store();
    let platform_records = summaries.platform_records();
    for record in [created, submitted] {
        platform_records
            .append(&input.new_run_id, &record, None)
            .await
            .map_err(|err| Error::engine_with_source("run store operation failed", err))?;
    }
    summaries.notify_platform_record(input.new_run_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use fabro_types::{FailureReason, Graph, PetriAdmission, RunSpec, WorkflowSettings, fixtures};

    use super::*;

    fn source(status: RunStatus) -> RunProjection {
        let mut projection = RunProjection::new(
            "Source title".to_string(),
            RunSpec {
                run_id:              fixtures::RUN_1,
                settings:            WorkflowSettings::default(),
                graph:               Graph::new("source"),
                graph_source:        Some("digraph source { start -> exit }".to_string()),
                workflow_slug:       Some("source".to_string()),
                workflow_version_id: None,
                target:              None,
                automation:          None,
                source_directory:    None,
                labels:              std::collections::HashMap::new(),
                provenance:          fabro_types::test_support::test_run_provenance(),
                definition_blob:     None,
                spec_blob:           None,
                git:                 None,
                fork_source_ref:     None,
                admission:           PetriAdmission::default(),
            },
            Utc::now(),
        );
        projection.status = status;
        projection.parent_id = Some(fixtures::RUN_2);
        projection
    }

    fn entry(ordinal: usize, sha: Option<&str>) -> TimelineEntry {
        TimelineEntry {
            ordinal,
            checkpoint_seq: 3,
            position: TimelinePosition {
                execution: 0,
                firing:    2,
                attempt:   1,
            },
            stage_id: Some("build@1".to_string()),
            node_name: "build".to_string(),
            visit: 1,
            workspace: None,
            run_commit_sha: sha.map(ToOwned::to_owned),
            diff_summary: None,
        }
    }

    #[test]
    fn the_record_carries_the_source_spec_under_the_new_id_and_names_the_source() {
        let source = source(RunStatus::Succeeded {
            reason: fabro_types::SuccessReason::Completed,
        });
        let record = forked_run_record(&ForkedRunInput {
            source:         &source,
            new_run_id:     fixtures::RUN_3,
            run_dir:        PathBuf::from("/tmp/unused"),
            checkpoint_sha: "abc".to_string(),
            provenance:     None,
            web_url:        Some("http://localhost/runs/x".to_string()),
            retried_from:   Some(fixtures::RUN_1),
        });
        assert_eq!(record.spec.run_id, fixtures::RUN_3);
        assert_eq!(
            record.spec.fork_source_ref,
            Some(ForkSourceRef {
                source_run_id:  fixtures::RUN_1,
                checkpoint_sha: "abc".to_string(),
            })
        );
        assert_eq!(record.spec.graph.name, "source");
        assert_eq!(record.title.as_deref(), Some("Source title"));
        assert_eq!(record.parent_id, Some(fixtures::RUN_2));
        assert_eq!(record.retried_from, Some(fixtures::RUN_1));
        assert_eq!(record.web_url.as_deref(), Some("http://localhost/runs/x"));
    }

    #[test]
    fn a_target_needs_a_commit() {
        let resolved = ResolvedForkTarget::of(&entry(2, Some("abc"))).unwrap();
        assert_eq!(resolved.response_target(), "@2");
        assert_eq!(resolved.checkpoint_sha, "abc");
        assert!(matches!(
            ResolvedForkTarget::of(&entry(2, None)),
            Err(Error::Validation(message)) if message.contains("no git_commit_sha")
        ));
    }

    #[test]
    fn a_rewind_or_retry_needs_a_terminal_source() {
        let running = source(RunStatus::Running);
        assert!(matches!(
            ensure_terminal(&running, &fixtures::RUN_1, "rewind"),
            Err(Error::Precondition(message)) if message.contains("must be terminal")
        ));
        for status in [
            RunStatus::Dead,
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
            RunStatus::Succeeded {
                reason: fabro_types::SuccessReason::Completed,
            },
        ] {
            ensure_terminal(&source(status), &fixtures::RUN_1, "retry").unwrap();
        }
        ensure_forkable(&running, &fixtures::RUN_1).unwrap();
    }
}
