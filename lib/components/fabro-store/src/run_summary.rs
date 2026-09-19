//! The run summary (`Run`) a projection stands for: the row the run list,
//! the board and the scheduler read, derived from the projection.

use fabro_types::{
    AskFabro, RepositoryRef, Run, RunId, RunLifecycle, RunLinks, RunModel, RunOrigin,
    RunProjection, RunSize, RunTimestamps, WorkflowRef, sum_usage,
};
use lithos_llm::types::Usage;

/// The run summary (`Run`) a projection stands for: what the run list, the
/// board and the scheduler read.
#[must_use]
pub fn build_summary(state: &RunProjection, run_id: &RunId) -> Run {
    let goal = state.spec.graph.goal().to_string();
    let diff_summary = state
        .conclusion
        .as_ref()
        .and_then(|conclusion| conclusion.diff.summary)
        .or_else(|| {
            state
                .checkpoints
                .iter()
                .rev()
                .find_map(|checkpoint| checkpoint.diff.summary)
        });

    let current_question = state
        .pending_interviews
        .iter()
        .min_by(|(left_id, left), (right_id, right)| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left_id.cmp(right_id))
        })
        .map(|(_, record)| record.question.clone());
    let models = run_models(state);
    let created_by = state.spec.provenance.subject.clone();
    let source_directory = state.spec.source_directory.clone();
    let repo_origin_url = state.spec.git.as_ref().map(|git| git.origin_url.clone());
    let start_time = state.start.as_ref().map(|start| start.start_time);
    let completed_at = state
        .conclusion
        .as_ref()
        .map(|conclusion| conclusion.timestamp);
    let run_timing = state
        .conclusion
        .as_ref()
        .map(|conclusion| conclusion.timing);
    let usage = projected_usage(state);

    Run {
        id: *run_id,
        parent_id: state.parent_id,
        children_count: 0,
        title: state.title().into_owned(),
        goal,
        workflow: WorkflowRef {
            slug:       state.spec.workflow_slug.clone(),
            name:       state.spec.workflow_name().map(ToOwned::to_owned),
            graph_name: state.spec.graph_name().map(ToOwned::to_owned),
            node_count: i64::try_from(state.spec.graph.nodes.len())
                .expect("graph node count should fit in i64"),
            edge_count: i64::try_from(state.spec.graph.edges.len())
                .expect("graph edge count should fit in i64"),
        },
        automation: state.spec.automation.clone(),
        repository: Some(RepositoryRef::from_origin_and_source(
            repo_origin_url,
            source_directory.as_deref(),
        )),
        created_by,
        origin: RunOrigin::default(),
        labels: state.spec.labels.clone(),
        lifecycle: RunLifecycle {
            status:          state.status,
            approval:        state.approval.clone(),
            pending_control: state.pending_control,
            queue_position:  None,
            error:           None,
            archived:        state.archived_at.is_some(),
            archived_at:     state.archived_at,
        },
        sandbox: state.sandbox.clone(),
        models,
        source_directory,
        timestamps: RunTimestamps {
            created_at: run_id.created_at(),
            started_at: start_time,
            last_event_at: Some(state.last_event_at),
            completed_at,
        },
        timing: run_timing,
        usage,
        size: RunSize::from_cost(usage.cost),
        ask_fabro: AskFabro::default(),
        diff: diff_summary,
        pull_request: state.pull_request.clone(),
        current_question,
        superseded_by: state.superseded_by,
        retried_from: state.retried_from,
        links: RunLinks {
            web: state.web_url.clone(),
        },
    }
}

/// The run's usage: the conclusion's total once the run ended, else the sum
/// of every non-boundary stage's usage so far.
#[must_use]
pub fn projected_usage(state: &RunProjection) -> Usage {
    if let Some(usage) = state
        .conclusion
        .as_ref()
        .and_then(|conclusion| conclusion.usage)
    {
        return usage;
    }

    sum_usage(
        state
            .iter_stages()
            .filter(|(stage_id, _)| !state.is_boundary_stage(stage_id.node_id()))
            .map(|(_, stage)| stage.usage),
    )
}

fn run_models(state: &RunProjection) -> Vec<RunModel> {
    let mut models = state
        .iter_stages()
        .filter_map(|(_, stage)| stage.model.as_ref())
        .map(|model| RunModel {
            provider: Some(model.provider.to_string()),
            name:     model.model_id.to_string(),
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.name.cmp(&right.name))
    });
    models.dedup_by(|left, right| left.provider == right.provider && left.name == right.name);
    models
}
