//! The checkpoint timeline of a run: every checkpoint Fabro recorded, at
//! its Petri position, with the commit it made and the stage it belongs
//! to, and the targets a fork names one of them by.

use std::collections::BTreeMap;
use std::str::FromStr;

use fabro_store::platform_records::{CheckpointRecord, DecisionRef};
use fabro_store::{PlatformRecord, StoredPlatformRecord};
use fabro_types::DiffSummary;

use crate::error::Error;

/// How a caller names a checkpoint: by ordinal (`@2`), by the latest visit
/// of a node (`build`), or by one visit of it (`build@1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkTarget {
    Ordinal(usize),
    LatestVisit(String),
    SpecificVisit(String, usize),
}

impl FromStr for ForkTarget {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        if let Some(rest) = s.strip_prefix('@') {
            let n: usize = rest
                .parse()
                .map_err(|_| Error::Validation(format!("invalid ordinal: @{rest}")))?;
            if n == 0 {
                return Err(Error::Validation("ordinal must be >= 1".to_string()));
            }
            return Ok(Self::Ordinal(n));
        }
        if let Some((name, visit)) = s.rsplit_once('@') {
            if !name.is_empty() && !visit.is_empty() {
                if let Ok(visit) = visit.parse::<usize>() {
                    if visit == 0 {
                        return Err(Error::Validation("visit number must be >= 1".to_string()));
                    }
                    return Ok(Self::SpecificVisit(name.to_string(), visit));
                }
            }
        }
        if s.trim().is_empty() {
            return Err(Error::Validation("a target names a checkpoint".to_string()));
        }
        Ok(Self::LatestVisit(s.to_string()))
    }
}

/// A stage as the run's projection labels it, by its Petri position: what
/// the timeline shows beside a checkpoint and what a target such as
/// `build@2` resolves through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageLabel {
    /// The stage id (`node@visit`) when the projection shows the stage.
    pub stage_id:  Option<String>,
    pub node_name: String,
    pub visit:     u32,
}

/// The stages of a run by `(execution, firing)`.
pub type StageLabels = BTreeMap<(u64, u64), StageLabel>;

/// The Petri position of a checkpoint: the attempt whose files it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelinePosition {
    pub execution: u64,
    pub firing:    u64,
    pub attempt:   u32,
}

/// One checkpoint of the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEntry {
    /// 1-based, in the order the checkpoints were recorded.
    pub ordinal:        usize,
    /// The checkpoint record's seq among the run's platform records.
    pub checkpoint_seq: u64,
    pub position:       TimelinePosition,
    /// The stage id (`node@visit`) when the projection shows the stage.
    pub stage_id:       Option<String>,
    pub node_name:      String,
    pub visit:          u32,
    /// The Petri workspace id the commit was made in.
    pub workspace:      Option<String>,
    pub run_commit_sha: Option<String>,
    pub diff_summary:   Option<DiffSummary>,
}

/// The run's checkpoints in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunTimeline {
    pub entries: Vec<TimelineEntry>,
}

impl RunTimeline {
    /// The timeline of `checkpoints` (the run's `checkpoint` platform
    /// records, in seq order), labelled through `labels`.
    #[must_use]
    pub fn build(checkpoints: &[StoredPlatformRecord], labels: &StageLabels) -> Self {
        let mut entries = Vec::new();
        for stored in checkpoints {
            let PlatformRecord::Checkpoint(record) = &stored.record else {
                continue;
            };
            let position = position_of(record);
            let label = labels.get(&(position.execution, position.firing));
            entries.push(TimelineEntry {
                ordinal: entries.len() + 1,
                checkpoint_seq: stored.seq,
                position,
                stage_id: label.and_then(|label| label.stage_id.clone()),
                node_name: label
                    .map(|label| label.node_name.clone())
                    .unwrap_or_default(),
                visit: label.map_or(1, |label| label.visit),
                workspace: record.workspace.clone(),
                run_commit_sha: record.git_commit_sha.clone(),
                diff_summary: record.diff_summary,
            });
        }
        Self { entries }
    }

    /// The latest checkpoint, the default target.
    pub fn latest(&self) -> Result<&TimelineEntry, Error> {
        self.entries
            .last()
            .ok_or_else(|| Error::Validation("the run has no checkpoint to fork at".to_string()))
    }

    /// The checkpoint `target` names.
    pub fn resolve(&self, target: &ForkTarget) -> Result<&TimelineEntry, Error> {
        match target {
            ForkTarget::Ordinal(n) => self
                .entries
                .iter()
                .find(|entry| entry.ordinal == *n)
                .ok_or_else(|| {
                    Error::Validation(format!(
                        "ordinal @{n} out of range (max @{})",
                        self.entries.len()
                    ))
                }),
            ForkTarget::LatestVisit(name) => self
                .entries
                .iter()
                .rev()
                .find(|entry| entry.node_name == *name)
                .ok_or_else(|| Error::Validation(format!("no checkpoint found for node '{name}'"))),
            ForkTarget::SpecificVisit(name, visit) => self
                .entries
                .iter()
                .find(|entry| {
                    entry.node_name == *name && usize::try_from(entry.visit) == Ok(*visit)
                })
                .ok_or_else(|| {
                    Error::Validation(format!("no visit {visit} found for node '{name}'"))
                }),
        }
    }

    /// The checkpoint `target` names, or the latest one.
    pub fn resolve_or_latest(&self, target: Option<&ForkTarget>) -> Result<&TimelineEntry, Error> {
        match target {
            Some(target) => self.resolve(target),
            None => self.latest(),
        }
    }
}

/// The position a checkpoint record names: its operation identity's
/// attempt, else the attempt it recorded, else the first.
fn position_of(record: &CheckpointRecord) -> TimelinePosition {
    let attempt = match record
        .operation
        .as_ref()
        .map(|operation| &operation.decision)
    {
        Some(DecisionRef::AttemptStart { attempt, .. } | DecisionRef::Route { attempt, .. }) => {
            *attempt
        }
        Some(DecisionRef::ExecutionStart) | None => record.attempt.unwrap_or(1),
    };
    TimelinePosition {
        execution: record.execution,
        firing: record.firing,
        attempt,
    }
}

#[cfg(test)]
mod tests {
    use fabro_store::platform_records::OperationKey;

    use super::*;

    fn checkpoint(
        seq: u64,
        execution: u64,
        firing: u64,
        sha: Option<&str>,
    ) -> StoredPlatformRecord {
        StoredPlatformRecord {
            seq,
            recorded_at: seq * 1_000,
            record: PlatformRecord::Checkpoint(CheckpointRecord {
                execution,
                firing,
                attempt: Some(1),
                workspace: Some("invocation-0-scope-0".to_string()),
                git_commit_sha: sha.map(ToOwned::to_owned),
                diff_summary: None,
                patch_blob: None,
                operation: Some(OperationKey {
                    execution,
                    decision: DecisionRef::AttemptStart { firing, attempt: 1 },
                    effect: "checkpoint".to_string(),
                }),
            }),
            position: None,
        }
    }

    fn label(node: &str, visit: u32) -> StageLabel {
        StageLabel {
            stage_id: Some(format!("{node}@{visit}")),
            node_name: node.to_string(),
            visit,
        }
    }

    fn timeline() -> RunTimeline {
        let labels: StageLabels = [
            ((0, 1), label("start", 1)),
            ((0, 2), label("build", 1)),
            ((0, 3), label("build", 2)),
        ]
        .into_iter()
        .collect();
        RunTimeline::build(
            &[
                checkpoint(7, 0, 1, Some("aaa")),
                checkpoint(9, 0, 2, Some("bbb")),
                checkpoint(11, 0, 3, Some("ccc")),
            ],
            &labels,
        )
    }

    #[test]
    fn a_target_parses_as_an_ordinal_a_node_or_a_visit() {
        assert_eq!("@4".parse::<ForkTarget>().unwrap(), ForkTarget::Ordinal(4));
        assert_eq!(
            "step2".parse::<ForkTarget>().unwrap(),
            ForkTarget::LatestVisit("step2".to_string())
        );
        assert_eq!(
            "build@2".parse::<ForkTarget>().unwrap(),
            ForkTarget::SpecificVisit("build".to_string(), 2)
        );
        assert!("@0".parse::<ForkTarget>().is_err());
        assert!("@x".parse::<ForkTarget>().is_err());
    }

    #[test]
    fn the_timeline_orders_checkpoints_and_labels_them() {
        let timeline = timeline();
        let ordinals: Vec<_> = timeline
            .entries
            .iter()
            .map(|entry| (entry.ordinal, entry.node_name.as_str(), entry.visit))
            .collect();
        assert_eq!(ordinals, [
            (1, "start", 1),
            (2, "build", 1),
            (3, "build", 2)
        ]);
        assert_eq!(timeline.entries[1].checkpoint_seq, 9);
        assert_eq!(timeline.entries[1].position, TimelinePosition {
            execution: 0,
            firing:    2,
            attempt:   1,
        });
        assert_eq!(timeline.entries[2].stage_id.as_deref(), Some("build@2"));
    }

    #[test]
    fn a_target_resolves_to_its_entry() {
        let timeline = timeline();
        assert_eq!(
            timeline.resolve(&ForkTarget::Ordinal(2)).unwrap().ordinal,
            2
        );
        assert_eq!(
            timeline
                .resolve(&ForkTarget::LatestVisit("build".to_string()))
                .unwrap()
                .ordinal,
            3
        );
        assert_eq!(
            timeline
                .resolve(&ForkTarget::SpecificVisit("build".to_string(), 1))
                .unwrap()
                .ordinal,
            2
        );
        assert_eq!(timeline.resolve_or_latest(None).unwrap().ordinal, 3);
        assert!(matches!(
            timeline.resolve(&ForkTarget::Ordinal(4)),
            Err(Error::Validation(message)) if message.contains("out of range")
        ));
        assert!(matches!(
            timeline.resolve(&ForkTarget::LatestVisit("test".to_string())),
            Err(Error::Validation(message)) if message.contains("no checkpoint found")
        ));
    }

    #[test]
    fn an_empty_timeline_has_no_latest() {
        assert!(RunTimeline::default().latest().is_err());
    }
}
