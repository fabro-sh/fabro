use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checkpoint::Checkpoint;
use crate::error::Result;
use crate::outcome::StageStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmoothnessRating {
    Effortless,
    Smooth,
    Bumpy,
    Struggled,
    Failed,
}

impl fmt::Display for SmoothnessRating {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SmoothnessRating::Effortless => "effortless",
            SmoothnessRating::Smooth => "smooth",
            SmoothnessRating::Bumpy => "bumpy",
            SmoothnessRating::Struggled => "struggled",
            SmoothnessRating::Failed => "failed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCategory {
    Repo,
    Code,
    Workflow,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub category: LearningCategory,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionKind {
    Retry,
    Timeout,
    WrongApproach,
    ToolFailure,
    Ambiguity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrictionPoint {
    pub kind: FrictionKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenItemKind {
    TechDebt,
    FollowUp,
    Investigation,
    TestGap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenItem {
    pub kind: OpenItemKind,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRetro {
    pub stage_id: String,
    pub stage_label: String,
    pub status: String,
    pub duration_ms: u64,
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateStats {
    pub total_duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    pub total_retries: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<String>,
    pub stages_completed: usize,
    pub stages_failed: usize,
}

/// Agent-generated qualitative narrative fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetroNarrative {
    pub smoothness: SmoothnessRating,
    pub intent: String,
    pub outcome: String,
    #[serde(default)]
    pub learnings: Vec<Learning>,
    #[serde(default)]
    pub friction_points: Vec<FrictionPoint>,
    #[serde(default)]
    pub open_items: Vec<OpenItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retro {
    pub run_id: String,
    pub workflow_name: String,
    pub goal: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothness: Option<SmoothnessRating>,
    pub stages: Vec<StageRetro>,
    pub stats: AggregateStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learnings: Option<Vec<Learning>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_points: Option<Vec<FrictionPoint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_items: Option<Vec<OpenItem>>,
}

impl Retro {
    /// Merge agent-generated narrative into this retro.
    pub fn apply_narrative(&mut self, narrative: RetroNarrative) {
        self.smoothness = Some(narrative.smoothness);
        self.intent = Some(narrative.intent);
        self.outcome = Some(narrative.outcome);
        self.learnings = if narrative.learnings.is_empty() {
            None
        } else {
            Some(narrative.learnings)
        };
        self.friction_points = if narrative.friction_points.is_empty() {
            None
        } else {
            Some(narrative.friction_points)
        };
        self.open_items = if narrative.open_items.is_empty() {
            None
        } else {
            Some(narrative.open_items)
        };
    }

    /// Save the retro as JSON to `run_dir/retro.json`.
    pub fn save(&self, run_dir: &Path) -> Result<()> {
        crate::save_json(self, &run_dir.join("retro.json"), "retro")
    }

    /// Load a retro from `run_dir/retro.json`.
    pub fn load(run_dir: &Path) -> Result<Self> {
        crate::load_json(&run_dir.join("retro.json"), "retro")
    }
}

/// Extract stage durations from `progress.jsonl` by reading `StageCompleted` events.
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
        let Some(duration_ms) = envelope.get("duration_ms").and_then(|v| v.as_u64()) else {
            continue;
        };
        durations.insert(name.to_string(), duration_ms);
    }
    durations
}

/// Build a `Retro` from checkpoint data and run metadata. All qualitative
/// fields (`smoothness`, `intent`, `outcome`, etc.) are left as `None` for
/// the retro agent to fill in.
#[allow(clippy::too_many_arguments)]
pub fn derive_retro(
    run_id: &str,
    workflow_name: &str,
    goal: &str,
    checkpoint: &Checkpoint,
    run_failed: bool,
    _run_error: Option<&str>,
    duration_ms: u64,
    stage_durations: &HashMap<String, u64>,
) -> Retro {
    let mut stages = Vec::new();
    let mut all_files: Vec<String> = Vec::new();
    let mut total_cost: Option<f64> = None;
    let mut total_retries: u32 = 0;
    let mut stages_completed: usize = 0;
    let mut stages_failed: usize = 0;

    for node_id in &checkpoint.completed_nodes {
        let outcome = checkpoint.node_outcomes.get(node_id);
        // node_retries stores attempts_used (1-indexed), convert to retry count
        let retries = checkpoint
            .node_retries
            .get(node_id)
            .copied()
            .unwrap_or(1)
            .saturating_sub(1);
        total_retries += retries;

        let status = outcome
            .map(|o| o.status.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        match outcome.map(|o| &o.status) {
            Some(StageStatus::Success | StageStatus::PartialSuccess) => stages_completed += 1,
            Some(StageStatus::Fail) => stages_failed += 1,
            _ => {}
        }

        let cost = outcome.and_then(|o| o.usage.as_ref()).and_then(|u| u.cost);
        if let Some(c) = cost {
            *total_cost.get_or_insert(0.0) += c;
        }

        let files = outcome.map(|o| o.files_touched.clone()).unwrap_or_default();
        all_files.extend(files.iter().cloned());

        stages.push(StageRetro {
            stage_id: node_id.clone(),
            stage_label: node_id.clone(),
            status,
            duration_ms: stage_durations.get(node_id).copied().unwrap_or(0),
            retries,
            cost,
            notes: outcome.and_then(|o| o.notes.clone()),
            failure_reason: outcome.and_then(|o| o.failure_reason().map(String::from)),
            files_touched: files,
        });
    }

    // If run failed with an error not captured in stages, record it
    if run_failed && stages_failed == 0 {
        stages_failed = 1;
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
        timestamp: Utc::now(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::Outcome;

    fn make_checkpoint_with_stages() -> Checkpoint {
        let mut node_outcomes = HashMap::new();
        let mut outcome_a = Outcome::success();
        outcome_a.notes = Some("Planned the approach".to_string());
        outcome_a.files_touched = vec!["src/main.rs".to_string()];
        outcome_a.usage = Some(crate::outcome::StageUsage {
            model: "claude-opus-4-6".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: Some(0.05),
        });
        node_outcomes.insert("plan".to_string(), outcome_a);

        let mut outcome_b = Outcome::success();
        outcome_b.files_touched = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
        outcome_b.usage = Some(crate::outcome::StageUsage {
            model: "claude-opus-4-6".to_string(),
            input_tokens: 2000,
            output_tokens: 1000,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            cost: Some(0.10),
        });
        node_outcomes.insert("code".to_string(), outcome_b);

        let mut node_retries = HashMap::new();
        // 2 attempts_used = 1 actual retry
        node_retries.insert("code".to_string(), 2u32);

        Checkpoint {
            timestamp: Utc::now(),
            current_node: "code".to_string(),
            completed_nodes: vec!["plan".to_string(), "code".to_string()],
            node_retries,
            context_values: HashMap::new(),
            logs: Vec::new(),
            node_outcomes,
            next_node_id: None,
            git_commit_sha: None,
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::new(),
        }
    }

    #[test]
    fn derive_retro_builds_stages_from_checkpoint() {
        let cp = make_checkpoint_with_stages();
        let durations: HashMap<String, u64> =
            [("plan".to_string(), 5000), ("code".to_string(), 15000)]
                .into_iter()
                .collect();

        let retro = derive_retro(
            "run-1",
            "my_pipeline",
            "Fix the bug",
            &cp,
            false,
            None,
            20000,
            &durations,
        );

        assert_eq!(retro.run_id, "run-1");
        assert_eq!(retro.workflow_name, "my_pipeline");
        assert_eq!(retro.goal, "Fix the bug");
        assert_eq!(retro.stages.len(), 2);
        assert_eq!(retro.stages[0].stage_id, "plan");
        assert_eq!(retro.stages[0].duration_ms, 5000);
        assert_eq!(retro.stages[0].retries, 0);
        assert_eq!(retro.stages[1].stage_id, "code");
        assert_eq!(retro.stages[1].duration_ms, 15000);
        assert_eq!(retro.stages[1].retries, 1);
        assert_eq!(retro.stats.total_duration_ms, 20000);
        assert_eq!(retro.stats.total_retries, 1);
        assert_eq!(retro.stats.stages_completed, 2);
        assert_eq!(retro.stats.stages_failed, 0);
        assert!((retro.stats.total_cost.unwrap() - 0.15).abs() < f64::EPSILON);
        assert_eq!(retro.stats.files_touched, vec!["src/lib.rs", "src/main.rs"]);
        assert!(retro.smoothness.is_none());
        assert!(retro.intent.is_none());
    }

    #[test]
    fn derive_retro_handles_failed_run() {
        let cp = Checkpoint {
            timestamp: Utc::now(),
            current_node: "start".to_string(),
            completed_nodes: vec!["start".to_string()],
            node_retries: HashMap::new(),
            context_values: HashMap::new(),
            logs: Vec::new(),
            node_outcomes: {
                let mut m = HashMap::new();
                m.insert("start".to_string(), Outcome::success());
                m
            },
            next_node_id: None,
            git_commit_sha: None,
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits: HashMap::new(),
        };

        let retro = derive_retro(
            "run-2",
            "pipe",
            "goal",
            &cp,
            true,
            Some("boom"),
            5000,
            &HashMap::new(),
        );

        assert_eq!(retro.stats.stages_failed, 1);
        assert_eq!(retro.stats.stages_completed, 1);
    }

    #[test]
    fn apply_narrative_merges_fields() {
        let cp = make_checkpoint_with_stages();
        let mut retro = derive_retro("r1", "p", "g", &cp, false, None, 1000, &HashMap::new());

        let narrative = RetroNarrative {
            smoothness: SmoothnessRating::Smooth,
            intent: "Fix authentication bug".to_string(),
            outcome: "Successfully fixed the login flow".to_string(),
            learnings: vec![Learning {
                category: LearningCategory::Code,
                text: "Token refresh logic was in the wrong module".to_string(),
            }],
            friction_points: vec![],
            open_items: vec![OpenItem {
                kind: OpenItemKind::TestGap,
                description: "No integration test for token refresh".to_string(),
            }],
        };

        retro.apply_narrative(narrative);

        assert_eq!(retro.smoothness, Some(SmoothnessRating::Smooth));
        assert_eq!(retro.intent.as_deref(), Some("Fix authentication bug"));
        assert_eq!(
            retro.outcome.as_deref(),
            Some("Successfully fixed the login flow")
        );
        assert_eq!(retro.learnings.as_ref().unwrap().len(), 1);
        assert!(retro.friction_points.is_none()); // empty vec -> None
        assert_eq!(retro.open_items.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cp = make_checkpoint_with_stages();
        let mut retro = derive_retro(
            "r1",
            "pipe",
            "goal",
            &cp,
            false,
            None,
            1000,
            &HashMap::new(),
        );
        retro.smoothness = Some(SmoothnessRating::Bumpy);
        retro.intent = Some("Test intent".to_string());

        retro.save(dir.path()).unwrap();
        let loaded = Retro::load(dir.path()).unwrap();

        assert_eq!(loaded.run_id, "r1");
        assert_eq!(loaded.smoothness, Some(SmoothnessRating::Bumpy));
        assert_eq!(loaded.intent.as_deref(), Some("Test intent"));
        assert_eq!(loaded.stages.len(), 2);
    }

    #[test]
    fn load_nonexistent_retro() {
        let result = Retro::load(Path::new("/nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn smoothness_rating_serde_roundtrip() {
        let json = serde_json::to_string(&SmoothnessRating::Effortless).unwrap();
        assert_eq!(json, "\"effortless\"");
        let parsed: SmoothnessRating = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SmoothnessRating::Effortless);
    }

    #[test]
    fn retro_narrative_serde() {
        let narrative = RetroNarrative {
            smoothness: SmoothnessRating::Failed,
            intent: "Deploy feature".to_string(),
            outcome: "Build failed".to_string(),
            learnings: vec![],
            friction_points: vec![FrictionPoint {
                kind: FrictionKind::ToolFailure,
                description: "Compiler error".to_string(),
                stage_id: Some("build".to_string()),
            }],
            open_items: vec![],
        };
        let json = serde_json::to_string(&narrative).unwrap();
        let parsed: RetroNarrative = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.smoothness, SmoothnessRating::Failed);
        assert_eq!(parsed.friction_points.len(), 1);
        assert_eq!(parsed.friction_points[0].kind, FrictionKind::ToolFailure);
    }

    #[test]
    fn extract_stage_durations_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("progress.jsonl");

        let event1 = serde_json::json!({
            "ts": "2025-01-01T00:00:00.000Z",
            "run_id": "r1",
            "event": "StageCompleted",
            "node_id": "plan",
            "node_label": "Plan",
            "stage_index": 0,
            "duration_ms": 5000,
            "status": "success",
            "preferred_label": null,
            "suggested_next_ids": [],
            "usage": null,
            "failure_reason": null,
            "notes": null,
            "files_touched": [],
            "attempt": 1,
            "max_attempts": 1,
            "failure_class": null
        });
        let event2 = serde_json::json!({
            "ts": "2025-01-01T00:00:05.000Z",
            "run_id": "r1",
            "event": "StageCompleted",
            "node_id": "code",
            "node_label": "Code",
            "stage_index": 1,
            "duration_ms": 15000,
            "status": "success",
            "preferred_label": null,
            "suggested_next_ids": [],
            "usage": null,
            "failure_reason": null,
            "notes": null,
            "files_touched": [],
            "attempt": 1,
            "max_attempts": 1,
            "failure_class": null
        });
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&event1).unwrap(),
            serde_json::to_string(&event2).unwrap()
        );
        std::fs::write(&jsonl, content).unwrap();

        let durations = extract_stage_durations(dir.path());
        assert_eq!(durations.get("plan"), Some(&5000));
        assert_eq!(durations.get("code"), Some(&15000));
    }

    #[test]
    fn extract_stage_durations_missing_file() {
        let durations = extract_stage_durations(Path::new("/nonexistent"));
        assert!(durations.is_empty());
    }
}
