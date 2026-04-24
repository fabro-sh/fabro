use fabro_store::Database;
use fabro_types::{ActorRef, RunId};
use tracing::error;

use super::fork::{self, ForkOutcome, ForkRunInput, ResolvedForkTarget};
use super::timeline::ForkTarget;
use super::{archive, run_git};
use crate::error::Error;
use crate::event::{self, Event};

#[derive(Debug, Clone)]
pub struct RewindInput {
    pub run_id: RunId,
    pub target: Option<ForkTarget>,
    pub push:   bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindOutcome {
    Full {
        source_run_id: RunId,
        new_run_id:    RunId,
        target:        ResolvedForkTarget,
    },
    Partial {
        source_run_id: RunId,
        new_run_id:    RunId,
        target:        ResolvedForkTarget,
        archive_error: String,
    },
}

pub async fn rewind(
    store: &Database,
    input: &RewindInput,
    actor: Option<ActorRef>,
) -> Result<RewindOutcome, Error> {
    let projection = run_git::load_projection(store, &input.run_id).await?;
    let current = projection.status.ok_or_else(|| {
        Error::Precondition(format!("run {} has no status; cannot rewind", input.run_id))
    })?;

    archive::ensure_not_archived(Some(current), &input.run_id)?;
    if current.terminal_status().is_none() {
        return Err(Error::Precondition(format!(
            "run {} must be terminal (succeeded, failed, or dead) to rewind; current status is {current}",
            input.run_id
        )));
    }

    let forked = fork::fork_run(store, &ForkRunInput {
        source_run_id: input.run_id,
        target:        input.target.clone(),
        push:          input.push,
    })
    .await?;

    match archive::archive(store, &input.run_id, actor).await {
        Ok(_) => {
            append_superseded_event_best_effort(store, &forked).await;
            Ok(RewindOutcome::Full {
                source_run_id: forked.source_run_id,
                new_run_id:    forked.new_run_id,
                target:        forked.target,
            })
        }
        Err(err) => Ok(RewindOutcome::Partial {
            source_run_id: forked.source_run_id,
            new_run_id:    forked.new_run_id,
            target:        forked.target,
            archive_error: err.to_string(),
        }),
    }
}

async fn append_superseded_event_best_effort(store: &Database, forked: &ForkOutcome) {
    let run_store = match store.open_run(&forked.source_run_id).await {
        Ok(run_store) => run_store,
        Err(err) => {
            error!(
                source_run_id = %forked.source_run_id,
                new_run_id = %forked.new_run_id,
                error = %err,
                "failed to open run for RunSupersededBy append after archive"
            );
            return;
        }
    };
    let event = Event::RunSupersededBy {
        new_run_id:                forked.new_run_id,
        target_checkpoint_ordinal: forked.target.checkpoint_ordinal,
        target_node_id:            forked.target.node_id.clone(),
        target_visit:              forked.target.visit,
    };
    if let Err(err) = event::append_event(&run_store, &forked.source_run_id, &event).await {
        error!(
            source_run_id = %forked.source_run_id,
            new_run_id = %forked.new_run_id,
            error = %err,
            "failed to append RunSupersededBy after archive"
        );
    }
}
