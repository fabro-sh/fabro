use std::collections::HashMap;
use std::path::Path;

pub use fabro_types::retro::{
    AggregateStats, FrictionKind, FrictionPoint, Learning, LearningCategory, OpenItem,
    OpenItemKind, Retro, RetroNarrative, SmoothnessRating, StageRetro,
};

#[derive(Debug, Clone)]
pub struct CompletedStage {
    pub node_id: String,
    pub status: String,
    pub succeeded: bool,
    pub failed: bool,
    pub retries: u32,
    pub cost: Option<f64>,
    pub notes: Option<String>,
    pub failure_reason: Option<String>,
    pub files_touched: Vec<String>,
}

pub trait RetroExt {
    fn save(&self, run_dir: &Path) -> anyhow::Result<()>;
    fn load(run_dir: &Path) -> anyhow::Result<Self>
    where
        Self: Sized;
}

impl RetroExt for Retro {
    fn save(&self, run_dir: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("retro serialize failed: {e}"))?;
        std::fs::write(run_dir.join("retro.json"), json)?;
        Ok(())
    }

    fn load(run_dir: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(run_dir.join("retro.json"))?;
        serde_json::from_str(&data).map_err(|e| anyhow::anyhow!("retro deserialize failed: {e}"))
    }
}

pub fn extract_stage_durations(run_dir: &Path) -> HashMap<String, u64> {
    let mut durations = HashMap::new();
    let jsonl_path = run_dir.join("progress.jsonl");
    let Ok(data) = std::fs::read_to_string(&jsonl_path) else {
        return durations;
    };
    for line in data.lines() {
        let Ok(envelope) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if envelope.get("event").and_then(|v| v.as_str()) != Some("StageCompleted") {
            continue;
        }
        let Some(name) = envelope.get("node_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(duration_ms) = envelope
            .get("duration_ms")
            .and_then(serde_json::Value::as_u64)
        else {
            continue;
        };
        durations.insert(name.to_string(), duration_ms);
    }
    durations
}

pub fn derive_retro(
    run_id: &str,
    workflow_name: &str,
    goal: &str,
    completed_stages: Vec<CompletedStage>,
    duration_ms: u64,
    stage_durations: &HashMap<String, u64>,
) -> Retro {
    let mut stages = Vec::new();
    let mut all_files: Vec<String> = Vec::new();
    let mut total_cost: Option<f64> = None;
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

        if let Some(c) = cs.cost {
            *total_cost.get_or_insert(0.0) += c;
        }

        let dur = stage_durations.get(&cs.node_id).copied().unwrap_or(0);

        stages.push(StageRetro {
            stage_label: cs.node_id.clone(),
            duration_ms: dur,
            retries: cs.retries,
            cost: cs.cost,
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
        total_cost,
        total_retries,
        files_touched: all_files,
        stages_completed,
        stages_failed,
    };

    Retro {
        run_id: run_id.to_string(),
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
