use std::collections::HashMap;

use lithos_llm::types::Usage;

use crate::usage::usage_is_empty;
use crate::{ModelRef, RunProjection, RunTiming, StageProjection, StageSummary, StageTiming};

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionUsageStage {
    pub node_id: String,
    pub usage:   Usage,
    /// Per-node timing summed across every visit of that node within this
    /// projection. `wall_time_ms`, `inference_time_ms`, `tool_time_ms`, and
    /// `active_time_ms` are all summed in lockstep.
    pub timing:  StageTiming,
    pub model:   Option<ModelRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionUsageByModel {
    pub model:  ModelRef,
    pub stages: i64,
    pub usage:  Usage,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectionUsageRollup {
    pub stages:            Vec<ProjectionUsageStage>,
    pub totals:            Usage,
    pub by_model:          Vec<ProjectionUsageByModel>,
    /// Run-level timing summed across every stage visit. `wall_time_ms` is
    /// the sum of stage visit wall times (not the run clock duration).
    pub timing:            RunTiming,
    /// Stage visits that used tokens or carried a cost.
    pub usage_visit_count: usize,
}

impl ProjectionUsageRollup {
    /// The totals, once at least one stage visit used tokens; `None` for a
    /// run that made no model calls.
    #[must_use]
    pub fn usage_if_present(&self) -> Option<Usage> {
        (self.usage_visit_count > 0).then_some(self.totals)
    }

    /// The conclusion's per-node summaries: one row per node the run
    /// visited, ordered by the node's first stage event, with the usage
    /// and timing summed over its visits and the retries counted past the
    /// first visit.
    #[must_use]
    pub fn conclusion_stages(&self, projection: &RunProjection) -> (Vec<StageSummary>, u32) {
        let projection_order = stage_projection_order(projection);
        let usage_by_node = self
            .stages
            .iter()
            .map(|stage| (stage.node_id.as_str(), stage))
            .collect::<HashMap<_, _>>();
        let mut nodes = projection
            .iter_stages()
            .map(|(stage_id, _)| stage_id.node_id())
            .collect::<Vec<_>>();
        nodes.dedup();
        let mut seen = std::collections::HashSet::new();
        let mut retries_sum: u32 = 0;
        let mut stage_rows = Vec::new();
        for node_id in nodes {
            if !seen.insert(node_id) {
                continue;
            }
            let visits = projection.list_node_visits(node_id).len();
            let retries = u32::try_from(visits.saturating_sub(1)).unwrap_or(u32::MAX);
            retries_sum = retries_sum.saturating_add(retries);
            let row = usage_by_node.get(node_id);
            let summary = StageSummary {
                stage_id: node_id.to_string(),
                stage_label: node_id.to_string(),
                timing: row.map_or_else(StageTiming::default, |stage| stage.timing),
                usage: row.map_or_else(Usage::default, |stage| stage.usage),
                retries,
            };
            stage_rows.push((
                projection_order.get(node_id).copied().unwrap_or(u32::MAX),
                summary,
            ));
        }
        stage_rows.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.stage_id.cmp(&right.1.stage_id))
        });
        let stages = stage_rows.into_iter().map(|(_, summary)| summary).collect();
        (stages, retries_sum)
    }
}

#[must_use]
pub fn usage_rollup_from_projection(projection: &RunProjection) -> ProjectionUsageRollup {
    let mut stage_indices = HashMap::<String, usize>::new();
    let mut stages = Vec::<ProjectionUsageStage>::new();
    let mut by_model = HashMap::<ModelRef, ProjectionUsageByModel>::new();
    let mut totals = Usage::default();
    let mut run_timing = RunTiming::default();
    let mut usage_visit_count = 0_usize;

    for (stage_id, stage) in projection.iter_stages() {
        if projection.is_boundary_stage(stage_id.node_id()) {
            continue;
        }
        let usage = stage.usage;
        if stage.completion.is_none() && stage.timing.is_none() && usage_is_empty(&usage) {
            continue;
        }

        let node_id = stage_id.node_id();
        let index = *stage_indices.entry(node_id.to_string()).or_insert_with(|| {
            let index = stages.len();
            stages.push(ProjectionUsageStage {
                node_id: node_id.to_string(),
                usage:   Usage::default(),
                timing:  StageTiming::default(),
                model:   None,
            });
            index
        });
        let row = &mut stages[index];

        if let Some(timing) = stage.timing {
            row.timing = row.timing.saturating_add(&timing);
            run_timing = run_timing.saturating_add(&RunTiming::from(timing));
        }

        if !usage_is_empty(&usage) {
            usage_visit_count += 1;
            row.usage = row.usage.saturating_add(usage);
            totals = totals.saturating_add(usage);

            if let Some(model) = &stage.model {
                row.model = Some(model.clone());
            }
            // A completed agent stage says which model used which tokens:
            // the root's route and each subagent's own. Until then, and for
            // a stage without a coding agent, `usage` goes under `model`.
            for (model, usage) in model_rows(stage) {
                let model_entry =
                    by_model
                        .entry(model.clone())
                        .or_insert_with(|| ProjectionUsageByModel {
                            model,
                            stages: 0,
                            usage: Usage::default(),
                        });
                model_entry.stages += 1;
                model_entry.usage = model_entry.usage.saturating_add(usage);
            }
        }
    }

    let mut by_model = by_model.into_values().collect::<Vec<_>>();
    by_model.sort_by(|left, right| left.model.sort_key().cmp(&right.model.sort_key()));

    ProjectionUsageRollup {
        stages,
        totals,
        by_model,
        timing: run_timing,
        usage_visit_count,
    }
}

/// The stage's usage by model: its `usage_by_model` rows when the stage
/// completed with them, else its `usage` under its `model`.
fn model_rows(stage: &StageProjection) -> Vec<(ModelRef, Usage)> {
    if stage.usage_by_model.is_empty() {
        return stage
            .model
            .iter()
            .map(|model| (model.clone(), stage.usage))
            .collect();
    }
    stage
        .usage_by_model
        .iter()
        .map(|row| (row.model.clone(), row.usage))
        .collect()
}

fn stage_projection_order(state: &RunProjection) -> HashMap<String, u32> {
    let mut order = HashMap::new();
    for (stage_id, stage) in state.iter_stages() {
        order
            .entry(stage_id.node_id().to_string())
            .and_modify(|first_seq: &mut u32| {
                *first_seq = (*first_seq).min(stage.first_event_seq.get());
            })
            .or_insert_with(|| stage.first_event_seq.get());
    }
    order
}
