use ::fabro_types::{ParallelBranchId, Principal, StageId, SystemActorKind};
use fabro_agent::AgentEvent;

use super::Event;
use crate::stage_scope::StageScope;

#[derive(Debug, Default)]
pub(super) struct StoredEventFields {
    pub(super) session_id:         Option<String>,
    pub(super) parent_session_id:  Option<String>,
    pub(super) node_id:            Option<String>,
    pub(super) node_label:         Option<String>,
    pub(super) stage_id:           Option<StageId>,
    pub(super) parallel_group_id:  Option<StageId>,
    pub(super) parallel_branch_id: Option<ParallelBranchId>,
    pub(super) tool_call_id:       Option<String>,
    pub(super) actor:              Option<Principal>,
}

fn default_node_label(node_id: Option<&String>, node_label: Option<String>) -> Option<String> {
    node_label.or_else(|| node_id.cloned())
}

fn node_stored_fields(node_id: Option<String>) -> StoredEventFields {
    let node_label = default_node_label(node_id.as_ref(), None);
    StoredEventFields {
        node_id,
        node_label,
        ..StoredEventFields::default()
    }
}

pub(super) fn stored_event_fields(event: &Event, scope: Option<&StageScope>) -> StoredEventFields {
    let mut fields = stored_event_fields_for_variant(event);
    if let Some(scope) = scope {
        if fields.node_id.is_none() {
            fields.node_id = Some(scope.node_id.clone());
            fields.node_label = default_node_label(Some(&scope.node_id), fields.node_label);
        }
        if fields.stage_id.is_none() {
            fields.stage_id = Some(StageId::new(scope.node_id.clone(), scope.visit));
        }
        if fields.parallel_group_id.is_none() {
            fields
                .parallel_group_id
                .clone_from(&scope.parallel_group_id);
        }
        if fields.parallel_branch_id.is_none() {
            fields
                .parallel_branch_id
                .clone_from(&scope.parallel_branch_id);
        }
    }
    fields
}

fn stored_event_fields_for_variant(event: &Event) -> StoredEventFields {
    match event {
        Event::RunCreated { provenance, .. } => StoredEventFields {
            actor: provenance.as_ref().and_then(|p| p.subject.clone()),
            ..StoredEventFields::default()
        },
        Event::RunCancelRequested { actor }
        | Event::RunPauseRequested { actor }
        | Event::RunUnpauseRequested { actor }
        | Event::RunArchived { actor }
        | Event::RunUnarchived { actor, .. }
        | Event::InterviewCompleted { actor, .. } => StoredEventFields {
            actor: actor.clone(),
            ..StoredEventFields::default()
        },
        Event::StageCompleted { node_id, name, .. }
        | Event::StageStarted { node_id, name, .. }
        | Event::StageRetrying { node_id, name, .. } => {
            let node_id_str = node_id.clone();
            let node_label = default_node_label(Some(&node_id_str), Some(name.clone()));
            StoredEventFields {
                node_id: Some(node_id_str),
                node_label,
                ..StoredEventFields::default()
            }
        }
        Event::StageFailed {
            node_id,
            name,
            actor,
            ..
        } => {
            let node_id_str = node_id.clone();
            let node_label = default_node_label(Some(&node_id_str), Some(name.clone()));
            StoredEventFields {
                node_id: Some(node_id_str),
                node_label,
                actor: actor.clone(),
                ..StoredEventFields::default()
            }
        }
        Event::ParallelStarted { node_id, visit, .. }
        | Event::ParallelCompleted { node_id, visit, .. } => {
            let node_id_str = node_id.clone();
            let node_label = default_node_label(Some(&node_id_str), None);
            let parallel_group_id = Some(StageId::new(node_id_str.clone(), *visit));
            StoredEventFields {
                node_id: Some(node_id_str),
                node_label,
                parallel_group_id,
                ..StoredEventFields::default()
            }
        }
        Event::CheckpointCompleted { node_id, .. }
        | Event::CheckpointFailed { node_id, .. }
        | Event::SubgraphStarted { node_id, .. }
        | Event::SubgraphCompleted { node_id, .. }
        | Event::ArtifactCaptured { node_id, .. }
        | Event::PromptCompleted { node_id, .. }
        | Event::CommandStarted { node_id, .. }
        | Event::CommandCompleted { node_id, .. }
        | Event::AgentCliStarted { node_id, .. }
        | Event::AgentCliCompleted { node_id, .. }
        | Event::AgentCliCancelled { node_id, .. }
        | Event::AgentCliTimedOut { node_id, .. } => node_stored_fields(Some(node_id.clone())),
        Event::Agent {
            stage,
            visit,
            event: agent_event,
            session_id,
            parent_session_id,
        } => {
            let node_id = Some(stage.clone());
            let node_label = default_node_label(node_id.as_ref(), None);
            let stage_id = Some(StageId::new(stage.clone(), *visit));
            let tool_call_id = agent_tool_call_id(agent_event).map(str::to_string);
            let actor = agent_actor_for_event(
                agent_event,
                session_id.as_deref(),
                parent_session_id.as_deref(),
            );
            StoredEventFields {
                session_id: session_id.clone(),
                parent_session_id: parent_session_id.clone(),
                node_id,
                node_label,
                stage_id,
                tool_call_id,
                actor,
                ..StoredEventFields::default()
            }
        }
        Event::GitCommit { node_id, .. } => node_stored_fields(node_id.clone()),
        Event::ParallelBranchStarted {
            parallel_group_id,
            parallel_branch_id,
            branch,
            ..
        }
        | Event::ParallelBranchCompleted {
            parallel_group_id,
            parallel_branch_id,
            branch,
            ..
        } => {
            let node_id = Some(branch.clone());
            let node_label = default_node_label(node_id.as_ref(), None);
            StoredEventFields {
                node_id,
                node_label,
                parallel_group_id: Some(parallel_group_id.clone()),
                parallel_branch_id: Some(parallel_branch_id.clone()),
                ..StoredEventFields::default()
            }
        }
        Event::Prompt { stage, .. }
        | Event::InterviewStarted { stage, .. }
        | Event::Failover { stage, .. } => node_stored_fields(Some(stage.clone())),
        Event::InterviewTimeout { actor, stage, .. }
        | Event::InterviewInterrupted { actor, stage, .. } => {
            let mut fields = node_stored_fields(Some(stage.clone()));
            fields.actor.clone_from(actor);
            fields
        }
        Event::StallWatchdogTimeout { node, .. } => {
            let mut fields = node_stored_fields(Some(node.clone()));
            fields.actor = Some(Principal::System {
                system_kind: SystemActorKind::Watchdog,
            });
            fields
        }
        _ => StoredEventFields::default(),
    }
}

fn agent_tool_call_id(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::ToolCallStarted { tool_call_id, .. }
        | AgentEvent::ToolCallCompleted { tool_call_id, .. } => Some(tool_call_id.as_str()),
        _ => None,
    }
}

fn agent_actor_for_event(
    event: &AgentEvent,
    session_id: Option<&str>,
    parent_session_id: Option<&str>,
) -> Option<Principal> {
    match event {
        AgentEvent::AssistantMessage { model, .. } => Some(Principal::Agent {
            session_id:        session_id.map(str::to_string),
            parent_session_id: parent_session_id.map(str::to_string),
            model:             Some(model.clone()),
        }),
        AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolCallOutputDelta { .. }
        | AgentEvent::ToolCallCompleted { .. } => Some(Principal::Agent {
            session_id:        session_id.map(str::to_string),
            parent_session_id: parent_session_id.map(str::to_string),
            model:             None,
        }),
        _ => None,
    }
}
