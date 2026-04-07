use fabro_types::RunId;
pub use fabro_types::retro::{
    AggregateStats, FrictionKind, FrictionPoint, Learning, LearningCategory, OpenItem,
    OpenItemKind, Retro, RetroNarrative, SmoothnessRating, StageRetro,
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CompletedStage {
    pub node_id: String,
    pub status: String,
    pub succeeded: bool,
    pub failed: bool,
    pub retries: u32,
    pub billing_usd_micros: Option<i64>,
    pub notes: Option<String>,
    pub failure_reason: Option<String>,
    pub files_touched: Vec<String>,
}

pub fn derive_retro(
    run_id: RunId,
    workflow_name: &str,
    goal: &str,
    completed_stages: Vec<CompletedStage>,
    duration_ms: u64,
    stage_durations: &HashMap<String, u64>,
) -> Retro {
    let mut stages = Vec::new();
    let mut all_files: Vec<String> = Vec::new();
    let mut total_billing_usd_micros: Option<i64> = None;
    let mut total_retries: u32 = 0;
    let mut stages_completed: usize = 0;
    let mut stages_failed: usize = 0;

    for cs in completed_stages {
        total_retries += cs.retries;

        if cs.succeeded {
            stages_completed += 1;
        }
        if cs.failed {
            stages_failed += 1;
        }

        if let Some(cost) = cs.billing_usd_micros {
            *total_billing_usd_micros.get_or_insert(0) += cost;
        }

        let dur = stage_durations.get(&cs.node_id).copied().unwrap_or(0);

        stages.push(StageRetro {
            stage_label: cs.node_id.clone(),
            duration_ms: dur,
            retries: cs.retries,
            billing_usd_micros: cs.billing_usd_micros,
            stage_id: cs.node_id,
            status: cs.status,
            notes: cs.notes,
            failure_reason: cs.failure_reason,
            files_touched: cs.files_touched,
        });

        all_files.extend(
            stages
                .last()
                .expect("stage just pushed")
                .files_touched
                .iter()
                .cloned(),
        );
    }

    all_files.sort();
    all_files.dedup();

    let stats = AggregateStats {
        total_duration_ms: duration_ms,
        total_billing_usd_micros,
        total_retries,
        files_touched: all_files,
        stages_completed,
        stages_failed,
    };

    Retro {
        run_id,
        workflow_name: workflow_name.to_string(),
        goal: goal.to_string(),
        timestamp: chrono::Utc::now(),
        smoothness: None,
        stages,
        stats,
        intent: None,
        outcome: None,
        learnings: None,
        friction_points: None,
        open_items: None,
    }
}
