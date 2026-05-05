use std::collections::{BTreeMap, HashMap};

use fabro_types::{BilledModelUsage, BilledTokenCounts, RunProjection};

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionBillingStage {
    pub node_id:     String,
    pub billing:     BilledTokenCounts,
    pub duration_ms: u64,
    pub model_id:    Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionBillingByModel {
    pub model_id: String,
    pub stages:   i64,
    pub billing:  BilledTokenCounts,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectionBillingRollup {
    pub stages:             Vec<ProjectionBillingStage>,
    pub totals:             BilledTokenCounts,
    pub by_model:           Vec<ProjectionBillingByModel>,
    pub runtime_ms:         u64,
    pub billed_visit_count: usize,
}

impl ProjectionBillingRollup {
    #[must_use]
    pub fn billing_if_present(&self) -> Option<BilledTokenCounts> {
        (self.billed_visit_count > 0).then(|| self.totals.clone())
    }
}

#[must_use]
pub fn billing_rollup_from_projection(projection: &RunProjection) -> ProjectionBillingRollup {
    let mut stage_indices = HashMap::<String, usize>::new();
    let mut stages = Vec::<ProjectionBillingStage>::new();
    let mut by_model = BTreeMap::<String, ProjectionBillingByModel>::new();
    let mut totals = BilledTokenCounts::default();
    let mut runtime_ms = 0_u64;
    let mut billed_visit_count = 0_usize;

    for (stage_id, stage) in projection.iter_stages() {
        if stage.completion.is_none() && stage.duration_ms.is_none() && stage.usage.is_none() {
            continue;
        }

        let node_id = stage_id.node_id();
        let index = *stage_indices.entry(node_id.to_string()).or_insert_with(|| {
            let index = stages.len();
            stages.push(ProjectionBillingStage {
                node_id:     node_id.to_string(),
                billing:     BilledTokenCounts::default(),
                duration_ms: 0,
                model_id:    None,
            });
            index
        });
        let row = &mut stages[index];

        if let Some(duration_ms) = stage.duration_ms {
            row.duration_ms = row.duration_ms.saturating_add(duration_ms);
            runtime_ms = runtime_ms.saturating_add(duration_ms);
        }

        if let Some(usage) = stage.usage.as_ref() {
            billed_visit_count += 1;
            row.model_id = Some(usage.model_id().to_string());
            accumulate_usage(&mut row.billing, usage);
            accumulate_usage(&mut totals, usage);

            let model_id = usage.model_id().to_string();
            let model_entry =
                by_model
                    .entry(model_id.clone())
                    .or_insert_with(|| ProjectionBillingByModel {
                        model_id,
                        stages: 0,
                        billing: BilledTokenCounts::default(),
                    });
            model_entry.stages += 1;
            accumulate_usage(&mut model_entry.billing, usage);
        }
    }

    ProjectionBillingRollup {
        stages,
        totals,
        by_model: by_model.into_values().collect(),
        runtime_ms,
        billed_visit_count,
    }
}

fn accumulate_usage(counts: &mut BilledTokenCounts, usage: &BilledModelUsage) {
    let tokens = usage.tokens();
    counts.input_tokens += tokens.input_tokens;
    counts.output_tokens += tokens.output_tokens;
    counts.reasoning_tokens += tokens.reasoning_tokens;
    counts.cache_read_tokens += tokens.cache_read_tokens;
    counts.cache_write_tokens += tokens.cache_write_tokens;
    counts.total_tokens += tokens.total_tokens();
    if let Some(value) = usage.total_usd_micros {
        *counts.total_usd_micros.get_or_insert(0) += value;
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::{BilledModelUsage, RunProjection, StageOutcome, first_event_seq};
    use serde_json::json;

    use super::billing_rollup_from_projection;

    fn test_usage(model_id: &str, input_tokens: i64, output_tokens: i64) -> BilledModelUsage {
        serde_json::from_value(json!({
            "input": {
                "usage": {
                    "model": {
                        "provider": "openai",
                        "model_id": model_id
                    },
                    "tokens": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens
                    }
                },
                "facts": {
                    "provider": "open_ai"
                }
            },
            "total_usd_micros": input_tokens + output_tokens
        }))
        .unwrap()
    }

    #[test]
    fn rollup_groups_stage_rows_by_node_and_sums_retry_visit_usage() {
        let mut projection = RunProjection::default();
        let failed_usage = test_usage("gpt-old", 100, 10);
        let success_usage = test_usage("gpt-new", 200, 20);
        let first = projection.stage_entry("verify", 1, first_event_seq(1));
        first.duration_ms = Some(1200);
        first.usage = Some(failed_usage);
        first.completion = Some(fabro_types::StageCompletion {
            outcome:        StageOutcome::Failed {
                retry_requested: true,
            },
            notes:          None,
            failure_reason: Some("try again".to_string()),
            timestamp:      chrono::Utc::now(),
        });
        let second = projection.stage_entry("verify", 2, first_event_seq(2));
        second.duration_ms = Some(800);
        second.usage = Some(success_usage);
        second.completion = Some(fabro_types::StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });

        let rollup = billing_rollup_from_projection(&projection);

        assert_eq!(rollup.stages.len(), 1);
        assert_eq!(rollup.stages[0].node_id, "verify");
        assert_eq!(rollup.stages[0].model_id.as_deref(), Some("gpt-new"));
        assert_eq!(rollup.stages[0].duration_ms, 2000);
        assert_eq!(rollup.stages[0].billing.input_tokens, 300);
        assert_eq!(rollup.stages[0].billing.output_tokens, 30);
        assert_eq!(rollup.stages[0].billing.total_usd_micros, Some(330));

        assert_eq!(rollup.runtime_ms, 2000);
        assert_eq!(rollup.totals.input_tokens, 300);
        assert_eq!(rollup.totals.output_tokens, 30);
        assert_eq!(rollup.totals.total_usd_micros, Some(330));
        assert_eq!(rollup.billed_visit_count, 2);

        assert_eq!(rollup.by_model.len(), 2);
        assert_eq!(rollup.by_model[0].model_id, "gpt-new");
        assert_eq!(rollup.by_model[0].stages, 1);
        assert_eq!(rollup.by_model[0].billing.input_tokens, 200);
        assert_eq!(rollup.by_model[1].model_id, "gpt-old");
        assert_eq!(rollup.by_model[1].stages, 1);
        assert_eq!(rollup.by_model[1].billing.input_tokens, 100);
    }

    #[test]
    fn rollup_includes_completed_non_llm_stage_rows_with_zero_billing() {
        let mut projection = RunProjection::default();
        let stage = projection.stage_entry("start", 1, first_event_seq(1));
        stage.duration_ms = Some(25);
        stage.completion = Some(fabro_types::StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });

        let rollup = billing_rollup_from_projection(&projection);

        assert_eq!(rollup.stages.len(), 1);
        assert_eq!(rollup.stages[0].node_id, "start");
        assert_eq!(rollup.stages[0].duration_ms, 25);
        assert!(rollup.stages[0].model_id.is_none());
        assert_eq!(rollup.stages[0].billing.input_tokens, 0);
        assert_eq!(rollup.runtime_ms, 25);
        assert!(rollup.by_model.is_empty());
        assert!(rollup.billing_if_present().is_none());
    }
}
