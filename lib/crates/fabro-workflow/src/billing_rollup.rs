use std::borrow::Cow;
use std::collections::HashMap;

use fabro_model::Catalog;
use fabro_types::{
    BilledTokenCounts, ModelRef, RunProjection, RunTiming, StageProjection, StageTiming,
};

fn stage_usage_with_cost<'a>(
    catalog: Option<&Catalog>,
    stage: &'a StageProjection,
) -> Cow<'a, BilledTokenCounts> {
    let Some(catalog) = catalog else {
        return Cow::Borrowed(&stage.usage);
    };
    let Some(model) = stage.model.as_ref() else {
        return Cow::Borrowed(&stage.usage);
    };
    if stage.usage.total_usd_micros.is_some() {
        return Cow::Borrowed(&stage.usage);
    }

    let Some(total_usd_micros) = catalog.price_tokens(model, &stage.usage.token_counts()) else {
        return Cow::Borrowed(&stage.usage);
    };
    let mut usage = stage.usage.clone();
    usage.total_usd_micros = Some(total_usd_micros);
    Cow::Owned(usage)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionBillingStage {
    pub node_id: String,
    pub billing: BilledTokenCounts,
    /// Per-node timing summed across every visit of that node within this
    /// projection. `wall_time_ms`, `inference_time_ms`, `tool_time_ms`, and
    /// `active_time_ms` are all summed in lockstep.
    pub timing:  StageTiming,
    pub model:   Option<ModelRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionBillingByModel {
    pub model:   ModelRef,
    pub stages:  i64,
    pub billing: BilledTokenCounts,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectionBillingRollup {
    pub stages:             Vec<ProjectionBillingStage>,
    pub totals:             BilledTokenCounts,
    pub by_model:           Vec<ProjectionBillingByModel>,
    /// Run-level timing summed across every stage visit. `wall_time_ms` is
    /// the sum of stage visit wall times (not the run clock duration).
    pub timing:             RunTiming,
    pub billed_visit_count: usize,
}

impl ProjectionBillingRollup {
    #[must_use]
    pub fn billing_if_present(&self) -> Option<BilledTokenCounts> {
        (self.billed_visit_count > 0).then(|| self.totals.clone())
    }
}

#[must_use]
pub fn billing_rollup_from_projection(
    projection: &RunProjection,
    catalog: Option<&Catalog>,
) -> ProjectionBillingRollup {
    let mut stage_indices = HashMap::<String, usize>::new();
    let mut stages = Vec::<ProjectionBillingStage>::new();
    let mut by_model = HashMap::<ModelRef, ProjectionBillingByModel>::new();
    let mut totals = BilledTokenCounts::default();
    let mut run_timing = RunTiming::default();
    let mut billed_visit_count = 0_usize;

    for (stage_id, stage) in projection.iter_stages() {
        if is_boundary_stage(projection, stage_id.node_id()) {
            continue;
        }
        let usage = stage_usage_with_cost(catalog, stage);
        let usage = usage.as_ref();
        if stage.completion.is_none() && stage.timing.is_none() && usage.is_zero() {
            continue;
        }

        let node_id = stage_id.node_id();
        let index = *stage_indices.entry(node_id.to_string()).or_insert_with(|| {
            let index = stages.len();
            stages.push(ProjectionBillingStage {
                node_id: node_id.to_string(),
                billing: BilledTokenCounts::default(),
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

        if !usage.is_zero() {
            billed_visit_count += 1;
            row.billing.add_counts(usage);
            totals.add_counts(usage);

            if let Some(model) = &stage.model {
                row.model = Some(model.clone());
                let model_entry =
                    by_model
                        .entry(model.clone())
                        .or_insert_with(|| ProjectionBillingByModel {
                            model:   model.clone(),
                            stages:  0,
                            billing: BilledTokenCounts::default(),
                        });
                model_entry.stages += 1;
                model_entry.billing.add_counts(usage);
            }
        }
    }

    let mut by_model = by_model.into_values().collect::<Vec<_>>();
    by_model.sort_by(|left, right| {
        let left_provider = left.model.provider.to_string();
        let right_provider = right.model.provider.to_string();
        left_provider
            .cmp(&right_provider)
            .then_with(|| left.model.model_id.cmp(&right.model.model_id))
            .then_with(|| {
                left.model
                    .speed
                    .map(<&'static str>::from)
                    .cmp(&right.model.speed.map(<&'static str>::from))
            })
    });

    ProjectionBillingRollup {
        stages,
        totals,
        by_model,
        timing: run_timing,
        billed_visit_count,
    }
}

fn is_boundary_stage(projection: &RunProjection, node_id: &str) -> bool {
    projection
        .spec()
        .graph()
        .nodes
        .get(node_id)
        .is_some_and(|node| matches!(node.handler_type(), Some("start" | "exit")))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_model::{Catalog, ModelRef, ProviderId};
    use fabro_types::{
        AttrValue, BilledTokenCounts, Graph, Node, RunProjection, RunSpec, StageCompletion,
        StageOutcome, WorkflowSettings, first_event_seq, fixtures,
    };

    use super::billing_rollup_from_projection;
    use crate::test_support::test_usage;

    fn test_projection() -> RunProjection {
        RunProjection::new(
            "Test run".to_string(),
            run_spec_with_boundary_nodes(),
            chrono::Utc::now(),
        )
    }

    #[test]
    fn rollup_groups_stage_rows_by_node_and_sums_retry_visit_usage() {
        let mut projection = test_projection();
        let failed_usage = test_usage("gpt-old", 100, 10);
        let success_usage = test_usage("gpt-new", 200, 20);
        let first = projection.stage_entry("verify", 1, first_event_seq(1));
        first.timing = Some(fabro_types::StageTiming::wall_only(1200));
        first.usage = BilledTokenCounts::from_billed_usage(std::slice::from_ref(&failed_usage));
        first.model = Some(failed_usage.model().clone());
        first.completion = Some(StageCompletion {
            outcome:        StageOutcome::Failed {
                retry_requested: true,
            },
            notes:          None,
            failure_reason: Some("try again".to_string()),
            timestamp:      chrono::Utc::now(),
        });
        let second = projection.stage_entry("verify", 2, first_event_seq(2));
        second.timing = Some(fabro_types::StageTiming::wall_only(800));
        second.usage = BilledTokenCounts::from_billed_usage(std::slice::from_ref(&success_usage));
        second.model = Some(success_usage.model().clone());
        second.completion = Some(StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });

        let rollup = billing_rollup_from_projection(&projection, None);

        assert_eq!(rollup.stages.len(), 1);
        assert_eq!(rollup.stages[0].node_id, "verify");
        assert_eq!(
            rollup.stages[0]
                .model
                .as_ref()
                .map(|model| model.model_id.as_str()),
            Some("gpt-new")
        );
        assert_eq!(rollup.stages[0].timing.wall_time_ms, 2000);
        assert_eq!(rollup.stages[0].billing.input_tokens, 300);
        assert_eq!(rollup.stages[0].billing.output_tokens, 30);
        assert_eq!(rollup.stages[0].billing.total_usd_micros, Some(330));

        assert_eq!(rollup.timing.wall_time_ms, 2000);
        assert_eq!(rollup.totals.input_tokens, 300);
        assert_eq!(rollup.totals.output_tokens, 30);
        assert_eq!(rollup.totals.total_usd_micros, Some(330));
        assert_eq!(rollup.billed_visit_count, 2);

        assert_eq!(rollup.by_model.len(), 2);
        assert_eq!(rollup.by_model[0].model.model_id, "gpt-new");
        assert_eq!(rollup.by_model[0].stages, 1);
        assert_eq!(rollup.by_model[0].billing.input_tokens, 200);
        assert_eq!(rollup.by_model[1].model.model_id, "gpt-old");
        assert_eq!(rollup.by_model[1].stages, 1);
        assert_eq!(rollup.by_model[1].billing.input_tokens, 100);
    }

    #[test]
    fn rollup_includes_completed_non_llm_stage_rows_with_zero_billing() {
        let mut projection = test_projection();
        let stage = projection.stage_entry("build", 1, first_event_seq(1));
        stage.timing = Some(fabro_types::StageTiming::wall_only(25));
        stage.completion = Some(StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });

        let rollup = billing_rollup_from_projection(&projection, None);

        assert_eq!(rollup.stages.len(), 1);
        assert_eq!(rollup.stages[0].node_id, "build");
        assert_eq!(rollup.stages[0].timing.wall_time_ms, 25);
        assert!(rollup.stages[0].model.is_none());
        assert_eq!(rollup.stages[0].billing.input_tokens, 0);
        assert_eq!(rollup.timing.wall_time_ms, 25);
        assert!(rollup.by_model.is_empty());
        assert!(rollup.billing_if_present().is_none());
    }

    #[test]
    fn rollup_excludes_workflow_boundary_stage_rows() {
        let mut projection = test_projection();
        projection.spec = run_spec_with_boundary_nodes();
        let start = projection.stage_entry("start", 1, first_event_seq(1));
        start.timing = Some(fabro_types::StageTiming::wall_only(25));
        start.completion = Some(StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });
        let exit = projection.stage_entry("exit", 1, first_event_seq(2));
        exit.timing = Some(fabro_types::StageTiming::wall_only(7));
        exit.completion = Some(StageCompletion {
            outcome:        StageOutcome::Succeeded,
            notes:          None,
            failure_reason: None,
            timestamp:      chrono::Utc::now(),
        });

        let rollup = billing_rollup_from_projection(&projection, None);

        assert_eq!(rollup.stages.len(), 0);
        assert_eq!(rollup.timing.wall_time_ms, 0);
    }

    #[test]
    fn rollup_prices_in_flight_stage_usage_using_catalog() {
        let mut projection = test_projection();
        let model = ModelRef {
            provider: ProviderId::openai(),
            model_id: "gpt-5.4".to_string(),
            speed:    None,
        };
        let stage = projection.stage_entry("agent", 1, first_event_seq(1));
        stage.started_at = Some(chrono::Utc::now());
        stage.usage = BilledTokenCounts {
            input_tokens: 500_000,
            output_tokens: 125_000,
            total_tokens: 625_000,
            ..BilledTokenCounts::default()
        };
        stage.model = Some(model.clone());

        let priced = billing_rollup_from_projection(&projection, Some(Catalog::builtin()));
        let unpriced = billing_rollup_from_projection(&projection, None);

        assert_eq!(priced.stages.len(), 1);
        assert_eq!(priced.stages[0].node_id, "agent");
        let stage_cost = priced.stages[0].billing.total_usd_micros;
        assert!(
            stage_cost.is_some_and(|cost| cost > 0),
            "expected priced stage cost, got {stage_cost:?}"
        );
        assert_eq!(priced.totals.total_usd_micros, stage_cost);
        assert_eq!(priced.by_model.len(), 1);
        assert_eq!(priced.by_model[0].billing.total_usd_micros, stage_cost);
        assert_eq!(unpriced.stages.len(), 1);
        assert_eq!(unpriced.stages[0].billing.total_usd_micros, None);
        assert_eq!(unpriced.totals.total_usd_micros, None);
    }

    fn run_spec_with_boundary_nodes() -> RunSpec {
        let mut graph = Graph::new("test");
        graph.nodes.insert("start".to_string(), {
            let mut node = Node::new("start");
            node.attrs.insert(
                "shape".to_string(),
                AttrValue::String("Mdiamond".to_string()),
            );
            node
        });
        graph.nodes.insert("exit".to_string(), {
            let mut node = Node::new("exit");
            node.attrs.insert(
                "shape".to_string(),
                AttrValue::String("Msquare".to_string()),
            );
            node
        });

        RunSpec {
            run_id: fixtures::RUN_1,
            settings: WorkflowSettings::default(),
            graph,
            graph_source: None,
            workflow_slug: None,
            automation: None,
            source_directory: None,
            labels: HashMap::new(),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
            git: None,
            fork_source_ref: None,
        }
    }
}
