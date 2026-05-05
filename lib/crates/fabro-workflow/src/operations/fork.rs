use anyhow::Result as AnyResult;
use fabro_store::{Database, RunProjection, RunProjectionReducer};
use fabro_types::{EventBody, EventEnvelope, ForkSourceRef, RunId};

use super::timeline::{ForkTarget, RunTimeline, TimelineEntry, build_timeline};
use crate::error::Error;
use crate::event::{self, Event};
use crate::records::{Checkpoint, RunSpec};

#[derive(Debug, Clone)]
pub struct ForkRunInput {
    pub source_run_id: RunId,
    pub target:        Option<ForkTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedForkTarget {
    pub checkpoint_ordinal: usize,
    pub node_id:            String,
    pub visit:              usize,
}

impl ResolvedForkTarget {
    #[must_use]
    pub fn response_target(&self) -> String {
        format!("@{}", self.checkpoint_ordinal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub source_run_id: RunId,
    pub new_run_id:    RunId,
    pub target:        ResolvedForkTarget,
}

pub async fn fork_run(
    store: &Database,
    input: &ForkRunInput,
) -> std::result::Result<ForkOutcome, Error> {
    let source_run_id = input.source_run_id;
    let run_store = store
        .open_run(&source_run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let state = run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let timeline = build_timeline(&state).map_err(|err| Error::engine(err.to_string()))?;
    let entry = resolve_fork_entry(&timeline, &source_run_id, input.target.as_ref())
        .map_err(|err| Error::Validation(err.to_string()))?;
    let checkpoint_sha = entry.run_commit_sha.clone().ok_or_else(|| {
        Error::Validation(format!(
            "checkpoint @{} has no git_commit_sha; cannot fork",
            entry.ordinal
        ))
    })?;

    validate_source_spec(state.spec.as_ref(), &checkpoint_sha)?;

    let events = run_store
        .list_events()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let historical_events = events
        .into_iter()
        .filter(|event| event.seq <= entry.checkpoint_seq)
        .collect::<Vec<_>>();
    let mut projection = RunProjection::apply_events(&historical_events)
        .map_err(|err| Error::engine(err.to_string()))?;
    let mut run_spec = projection
        .spec
        .clone()
        .ok_or_else(|| Error::engine("source run projection has no spec"))?;

    let new_run_id = RunId::new();
    run_spec.run_id = new_run_id;
    run_spec.fork_source_ref = Some(ForkSourceRef {
        source_run_id,
        checkpoint_sha: checkpoint_sha.clone(),
    });
    projection.spec = Some(run_spec);
    projection.start = None;
    projection.sandbox = None;
    projection.conclusion = None;
    projection.retro = None;
    projection.retro_prompt = None;
    projection.retro_response = None;
    projection.final_patch = None;
    projection.pull_request = None;
    projection.superseded_by = None;
    if let Some(checkpoint) = projection.checkpoint.as_mut() {
        checkpoint.git_commit_sha = Some(checkpoint_sha);
    }

    persist_forked_run(store, &projection, &historical_events).await?;

    Ok(ForkOutcome {
        source_run_id,
        new_run_id,
        target: ResolvedForkTarget {
            checkpoint_ordinal: entry.ordinal,
            node_id:            entry.node_name.clone(),
            visit:              entry.visit,
        },
    })
}

fn validate_source_spec(
    spec: Option<&RunSpec>,
    checkpoint_sha: &str,
) -> std::result::Result<(), Error> {
    let spec = spec.ok_or_else(|| Error::engine("source run projection has no spec"))?;
    if spec.in_place {
        return Err(Error::Validation(
            "source run was created with --in-place; cannot fork (no git checkpoint history)"
                .to_string(),
        ));
    }
    if checkpoint_sha.trim().is_empty() {
        return Err(Error::Validation(
            "target checkpoint has an empty git_commit_sha; cannot fork".to_string(),
        ));
    }
    let Some(origin) = spec.repo_origin_url() else {
        return Err(Error::Validation(
            "source run has no repo_origin_url; cannot validate fork origin".to_string(),
        ));
    };
    if fabro_github::normalize_repo_origin_url(origin).is_empty() {
        return Err(Error::Validation(
            "source run has an empty repo_origin_url; cannot validate fork origin".to_string(),
        ));
    }
    Ok(())
}

fn resolve_fork_entry<'a>(
    timeline: &'a RunTimeline,
    source_run_id: &RunId,
    target: Option<&ForkTarget>,
) -> AnyResult<&'a TimelineEntry> {
    match target {
        Some(target) => timeline.resolve(target),
        None => timeline
            .entries
            .last()
            .ok_or_else(|| anyhow::anyhow!("no checkpoints found for run {source_run_id}")),
    }
}

async fn persist_forked_run(
    store: &Database,
    projection: &RunProjection,
    historical_events: &[EventEnvelope],
) -> std::result::Result<(), Error> {
    let spec = projection
        .spec
        .as_ref()
        .ok_or_else(|| Error::engine("forked run projection has no spec"))?;
    let checkpoint = projection
        .checkpoint
        .as_ref()
        .ok_or_else(|| Error::engine("forked run projection has no checkpoint"))?;

    let run_store = store
        .create_run(&spec.run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;

    event::append_event(&run_store, &spec.run_id, &Event::RunCreated {
        run_id:           spec.run_id,
        settings:         serde_json::to_value(&spec.settings)
            .map_err(|err| Error::engine(err.to_string()))?,
        graph:            serde_json::to_value(&spec.graph)
            .map_err(|err| Error::engine(err.to_string()))?,
        workflow_source:  projection.graph_source.clone(),
        workflow_config:  None,
        labels:           spec.labels.clone().into_iter().collect(),
        run_dir:          String::new(),
        source_directory: spec.source_directory.clone(),
        workflow_slug:    spec.workflow_slug.clone(),
        db_prefix:        None,
        provenance:       spec.provenance.clone(),
        manifest_blob:    spec.manifest_blob,
        git:              spec.git.clone(),
        fork_source_ref:  spec.fork_source_ref.clone(),
        in_place:         spec.in_place,
        web_url:          None,
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))?;

    let replayed_checkpoint =
        replay_historical_projection_events(&run_store, spec.run_id, historical_events).await?;
    if !replayed_checkpoint {
        event::append_event(
            &run_store,
            &spec.run_id,
            &checkpoint_completed_event(checkpoint),
        )
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    }
    event::append_event(&run_store, &spec.run_id, &Event::RunSubmitted {
        definition_blob: spec.definition_blob,
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))
}

async fn replay_historical_projection_events(
    run_store: &fabro_store::RunDatabase,
    new_run_id: RunId,
    historical_events: &[EventEnvelope],
) -> std::result::Result<bool, Error> {
    let mut replayed_checkpoint = false;
    for envelope in historical_events {
        if !replay_event_for_fork_projection(&envelope.event.body) {
            continue;
        }
        if matches!(envelope.event.body, EventBody::CheckpointCompleted(_)) {
            replayed_checkpoint = true;
        }
        let mut event = envelope.event.clone();
        event.id = format!("{new_run_id}-fork-{}", envelope.seq);
        event.run_id = new_run_id;
        let payload = event::build_redacted_event_payload(&event, &new_run_id)
            .map_err(|err| Error::engine(err.to_string()))?;
        run_store
            .append_event(&payload)
            .await
            .map_err(|err| Error::engine(err.to_string()))?;
    }
    Ok(replayed_checkpoint)
}

fn replay_event_for_fork_projection(body: &EventBody) -> bool {
    matches!(
        body,
        EventBody::StageCompleted(_)
            | EventBody::StageFailed(_)
            | EventBody::StagePrompt(_)
            | EventBody::PromptCompleted(_)
            | EventBody::CheckpointCompleted(_)
            | EventBody::InterviewStarted(_)
            | EventBody::InterviewCompleted(_)
            | EventBody::InterviewTimeout(_)
            | EventBody::InterviewInterrupted(_)
            | EventBody::AgentSessionStarted(_)
            | EventBody::AgentCliStarted(_)
            | EventBody::AgentCliCancelled(_)
            | EventBody::AgentCliTimedOut(_)
            | EventBody::CommandStarted(_)
            | EventBody::CommandCompleted(_)
            | EventBody::ParallelCompleted(_)
    )
}

fn checkpoint_completed_event(checkpoint: &Checkpoint) -> Event {
    let status = checkpoint
        .node_outcomes
        .get(&checkpoint.current_node)
        .map_or_else(
            || "success".to_string(),
            |outcome| outcome.status.to_string(),
        );

    Event::CheckpointCompleted {
        node_id: checkpoint.current_node.clone(),
        status,
        current_node: checkpoint.current_node.clone(),
        completed_nodes: checkpoint.completed_nodes.clone(),
        node_retries: checkpoint.node_retries.clone().into_iter().collect(),
        context_values: checkpoint.context_values.clone().into_iter().collect(),
        node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
        next_node_id: checkpoint.next_node_id.clone(),
        git_commit_sha: checkpoint.git_commit_sha.clone(),
        loop_failure_signatures: checkpoint
            .loop_failure_signatures
            .iter()
            .map(|(signature, count)| (signature.to_string(), *count))
            .collect(),
        restart_failure_signatures: checkpoint
            .restart_failure_signatures
            .iter()
            .map(|(signature, count)| (signature.to_string(), *count))
            .collect(),
        node_visits: checkpoint.node_visits.clone().into_iter().collect(),
        diff: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_graphviz::graph::Graph;
    use fabro_store::{Database, RunProjectionReducer};
    use fabro_types::{StageId, WorkflowSettings, fixtures};
    use object_store::memory::InMemory;

    use super::*;

    fn test_store() -> Database {
        Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        )
    }

    #[tokio::test]
    async fn fork_persists_historical_node_projection_through_target_checkpoint() {
        let store = test_store();
        let source_run_id = fixtures::RUN_1;
        let source = store.create_run(&source_run_id).await.unwrap();
        let graph = Graph::new("fork-source");
        let settings = WorkflowSettings::default();

        event::append_event(&source, &source_run_id, &Event::RunCreated {
            run_id:           source_run_id,
            settings:         serde_json::to_value(&settings).unwrap(),
            graph:            serde_json::to_value(&graph).unwrap(),
            workflow_source:  Some("digraph fork_source {}".to_string()),
            workflow_config:  None,
            labels:           BTreeMap::new(),
            run_dir:          "/tmp/source".to_string(),
            source_directory: Some("/client/source".to_string()),
            workflow_slug:    Some("fork-source".to_string()),
            db_prefix:        None,
            provenance:       None,
            manifest_blob:    None,
            git:              Some(fabro_types::GitContext {
                origin_url:   "https://github.com/example/repo.git".to_string(),
                branch:       "main".to_string(),
                sha:          None,
                dirty:        fabro_types::DirtyStatus::Clean,
                push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
            }),
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        })
        .await
        .unwrap();

        let mut node_visits = BTreeMap::new();
        node_visits.insert("work".to_string(), 1);
        event::append_event(&source, &source_run_id, &Event::StageCompleted {
            node_id: "work".to_string(),
            name: "Work".to_string(),
            index: 1,
            duration_ms: 10,
            status: "succeeded".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: None,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: Some(node_visits.clone()),
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: Some("historical response".to_string()),
            attempt: 1,
            max_attempts: 1,
        })
        .await
        .unwrap();

        event::append_event(&source, &source_run_id, &Event::CheckpointCompleted {
            node_id: "work".to_string(),
            status: "succeeded".to_string(),
            current_node: "work".to_string(),
            completed_nodes: vec!["work".to_string()],
            node_retries: BTreeMap::new(),
            context_values: BTreeMap::new(),
            node_outcomes: BTreeMap::new(),
            next_node_id: None,
            git_commit_sha: Some("abc123".to_string()),
            loop_failure_signatures: BTreeMap::new(),
            restart_failure_signatures: BTreeMap::new(),
            node_visits,
            diff: None,
        })
        .await
        .unwrap();

        let outcome = fork_run(&store, &ForkRunInput {
            source_run_id,
            target: None,
        })
        .await
        .unwrap();

        let forked = store.open_run(&outcome.new_run_id).await.unwrap();
        let forked_events = forked.list_events().await.unwrap();
        let forked_state = fabro_store::RunProjection::apply_events(&forked_events).unwrap();
        let node = forked_state
            .stage(&StageId::new("work", 1))
            .expect("forked state should retain historical node projection");

        assert_eq!(node.response.as_deref(), Some("historical response"));
        assert_eq!(forked_state.checkpoints.len(), 1);
        assert_eq!(
            forked_state
                .spec
                .unwrap()
                .fork_source_ref
                .unwrap()
                .source_run_id,
            source_run_id
        );
    }
}
