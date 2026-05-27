use std::collections::BTreeMap;

use fabro_store::Database;
use fabro_types::{RunId, RunProvenance, RunSpec, RunStatus};

use super::archive::ensure_not_archived;
use super::run_store::map_open_run_error;
use crate::error::Error;
use crate::event::{self, Event};

#[derive(Debug, Clone)]
pub struct RetryRunInput {
    pub source_run_id: RunId,
    pub new_run_id:    RunId,
    pub provenance:    Option<RunProvenance>,
    pub web_url:       Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryOutcome {
    pub source_run_id: RunId,
    pub new_run_id:    RunId,
}

pub async fn retry_run(
    store: &Database,
    input: &RetryRunInput,
) -> std::result::Result<RetryOutcome, Error> {
    let source_run_id = input.source_run_id;
    let new_run_id = input.new_run_id;
    let source_store = store
        .open_run(&source_run_id)
        .await
        .map_err(|err| map_open_run_error(&source_run_id, err))?;
    let source = source_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;

    ensure_not_archived(source.archived_at.is_some(), &source_run_id)?;
    ensure_retryable(source.status, &source_run_id)?;

    let title = source.title().into_owned();
    let parent_id = source.parent_id;
    let RunSpec {
        run_id: _,
        settings,
        graph,
        graph_source,
        workflow_slug,
        automation,
        source_directory,
        labels,
        provenance: _,
        manifest_blob,
        definition_blob,
        git,
        fork_source_ref,
    } = source.spec;

    let settings = serde_json::to_value(&settings).map_err(|err| Error::engine(err.to_string()))?;
    let graph = serde_json::to_value(&graph).map_err(|err| Error::engine(err.to_string()))?;

    let retry_store = store
        .create_run(&new_run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;

    event::append_event(&retry_store, &new_run_id, &Event::RunCreated {
        run_id: new_run_id,
        title: Some(title),
        settings,
        graph,
        workflow_source: graph_source,
        workflow_config: None,
        labels: labels.into_iter().collect::<BTreeMap<_, _>>(),
        run_dir: String::new(),
        source_directory,
        workflow_slug,
        automation,
        db_prefix: None,
        provenance: input.provenance.clone(),
        manifest_blob,
        git,
        fork_source_ref,
        retried_from: Some(source_run_id),
        parent_id,
        web_url: input.web_url.clone(),
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))?;

    event::append_event(&retry_store, &new_run_id, &Event::RunSubmitted {
        definition_blob,
    })
    .await
    .map_err(|err| Error::engine(err.to_string()))?;

    Ok(RetryOutcome {
        source_run_id,
        new_run_id,
    })
}

fn ensure_retryable(status: RunStatus, run_id: &RunId) -> std::result::Result<(), Error> {
    match status {
        RunStatus::Failed { .. } | RunStatus::Dead => Ok(()),
        other => Err(Error::Precondition(format!(
            "run {run_id} cannot be retried from status {other}; expected failed or dead"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_store::{Database, RunProjectionReducer};
    use fabro_types::{
        AuthMethod, DirtyStatus, FailureReason, ForkSourceRef, GitContext, Graph, IdpIdentity,
        PreRunPushOutcome, Principal, PullRequestLink, RunBlobId, RunRunnableSource,
        RunServerProvenance, RunTiming, UserPrincipal, WorkflowSettings, fixtures,
    };
    use object_store::memory::InMemory;

    use super::*;

    fn memory_store() -> Database {
        Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        )
    }

    fn actor(login: &str) -> Principal {
        Principal::User(UserPrincipal {
            identity:    IdpIdentity::new("github", format!("user:{login}")).unwrap(),
            login:       login.to_string(),
            auth_method: AuthMethod::DevToken,
            avatar_url:  None,
        })
    }

    fn provenance(login: &str) -> RunProvenance {
        RunProvenance {
            server:  Some(RunServerProvenance {
                version: "test".to_string(),
            }),
            client:  None,
            subject: Some(actor(login)),
        }
    }

    fn git_context() -> GitContext {
        GitContext {
            origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:       "main".to_string(),
            sha:          Some("abc123".to_string()),
            dirty:        DirtyStatus::Clean,
            push_outcome: PreRunPushOutcome::NotAttempted,
        }
    }

    async fn append_created(
        store: &fabro_store::RunDatabase,
        run_id: RunId,
        manifest_blob: Option<RunBlobId>,
        fork_source_ref: Option<ForkSourceRef>,
    ) {
        let mut settings = WorkflowSettings::default();
        settings
            .run
            .metadata
            .insert("env".to_string(), "test".to_string());
        let labels = HashMap::from([("team".to_string(), "core".to_string())]);
        event::append_event(store, &run_id, &Event::RunCreated {
            run_id,
            title: Some("Original title".to_string()),
            settings: serde_json::to_value(&settings).unwrap(),
            graph: serde_json::to_value(Graph::new("retry_source")).unwrap(),
            workflow_source: Some("digraph retry_source { start -> exit }".to_string()),
            workflow_config: None,
            labels: labels.into_iter().collect(),
            run_dir: "/tmp/source".to_string(),
            source_directory: Some("/workspace/source".to_string()),
            workflow_slug: Some("retry-source".to_string()),
            automation: None,
            db_prefix: None,
            provenance: Some(provenance("source-user")),
            manifest_blob,
            git: Some(git_context()),
            fork_source_ref,
            retried_from: None,
            parent_id: None,
            web_url: None,
        })
        .await
        .unwrap();
    }

    async fn append_runnable(store: &fabro_store::RunDatabase, run_id: RunId) {
        event::append_event(store, &run_id, &Event::RunRunnable {
            source: RunRunnableSource::StartRequested,
            actor:  None,
        })
        .await
        .unwrap();
    }

    async fn append_started(store: &fabro_store::RunDatabase, run_id: RunId) {
        append_runnable(store, run_id).await;
        event::append_event(store, &run_id, &Event::RunStarting)
            .await
            .unwrap();
        event::append_event(store, &run_id, &Event::RunRunning)
            .await
            .unwrap();
    }

    async fn append_failed(store: &fabro_store::RunDatabase, run_id: RunId, reason: FailureReason) {
        append_started(store, run_id).await;
        let event = Event::workflow_run_failed_from_error(
            &Error::engine("boom"),
            RunTiming::wall_only(10),
            reason,
            None,
            None,
            None,
            None,
        );
        event::append_event(store, &run_id, &event).await.unwrap();
    }

    async fn seed_retryable_failed_source(
        store: &Database,
        source_run_id: RunId,
    ) -> (Option<RunBlobId>, Option<RunBlobId>, ForkSourceRef) {
        let source_store = store.create_run(&source_run_id).await.unwrap();
        let manifest_blob = Some(
            source_store
                .write_blob(br#"{\"manifest\":true}"#)
                .await
                .unwrap(),
        );
        let definition_blob = Some(
            source_store
                .write_blob(br#"{\"definition\":true}"#)
                .await
                .unwrap(),
        );
        let fork_source_ref = ForkSourceRef {
            source_run_id:  fixtures::RUN_3,
            checkpoint_sha: "fork-sha".to_string(),
        };
        append_created(
            &source_store,
            source_run_id,
            manifest_blob,
            Some(fork_source_ref.clone()),
        )
        .await;
        event::append_event(&source_store, &source_run_id, &Event::RunSubmitted {
            definition_blob,
        })
        .await
        .unwrap();
        event::append_event(&source_store, &source_run_id, &Event::RunParentLinked {
            previous_parent_id: None,
            parent_id:          fixtures::RUN_2,
            actor:              None,
        })
        .await
        .unwrap();
        event::append_event(&source_store, &source_run_id, &Event::RunTitleUpdated {
            title: "Current title".to_string(),
            actor: None,
        })
        .await
        .unwrap();
        event::append_event(&source_store, &source_run_id, &Event::CheckpointCompleted {
            node_id: "work".to_string(),
            status: "succeeded".to_string(),
            current_node: "work".to_string(),
            completed_nodes: vec!["work".to_string()],
            node_retries: BTreeMap::new(),
            context_values: BTreeMap::new(),
            node_outcomes: BTreeMap::new(),
            next_node_id: None,
            git_commit_sha: Some("checkpoint-sha".to_string()),
            loop_failure_signatures: BTreeMap::new(),
            restart_failure_signatures: BTreeMap::new(),
            node_visits: BTreeMap::new(),
            diff: Some("diff --git a/file b/file".to_string()),
            diff_summary: Some(fabro_types::DiffSummary {
                files_changed: 1,
                additions:     1,
                deletions:     0,
            }),
        })
        .await
        .unwrap();
        event::append_event(&source_store, &source_run_id, &Event::SandboxInitialized {
            provider:          fabro_types::SandboxProviderKind::Local,
            id:                "sandbox-source".to_string(),
            working_directory: "/tmp/source".to_string(),
            image:             None,
            snapshot:          None,
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
            workspace_root:    None,
            repos_root:        None,
            primary_repo_path: None,
            primary_repo_link: None,
        })
        .await
        .unwrap();
        event::append_event(&source_store, &source_run_id, &Event::PullRequestLinked {
            pull_request: PullRequestLink {
                owner:  "fabro-sh".to_string(),
                repo:   "fabro".to_string(),
                number: 42,
            },
        })
        .await
        .unwrap();
        append_failed(&source_store, source_run_id, FailureReason::WorkflowError).await;
        (manifest_blob, definition_blob, fork_source_ref)
    }

    #[tokio::test]
    async fn retry_creates_fresh_run_from_durable_definition_only() {
        let store = memory_store();
        let source_run_id = fixtures::RUN_1;
        let (manifest_blob, definition_blob, fork_source_ref) =
            seed_retryable_failed_source(&store, source_run_id).await;
        let source_event_count = store
            .open_run(&source_run_id)
            .await
            .unwrap()
            .list_events()
            .await
            .unwrap()
            .len();

        let outcome = retry_run(&store, &RetryRunInput {
            source_run_id,
            new_run_id: RunId::new(),
            provenance: Some(provenance("retry-user")),
            web_url: Some("http://localhost:3000/runs/retry".to_string()),
        })
        .await
        .unwrap();

        assert_ne!(outcome.new_run_id, source_run_id);
        assert_eq!(outcome.source_run_id, source_run_id);

        let retry_store = store.open_run(&outcome.new_run_id).await.unwrap();
        let retry_events = retry_store.list_events().await.unwrap();
        let retry_state = fabro_store::RunProjection::apply_events(&retry_events).unwrap();
        assert_eq!(retry_events.len(), 2);
        assert_eq!(retry_state.status, RunStatus::Submitted);
        assert_eq!(retry_state.retried_from, Some(source_run_id));
        assert_eq!(retry_state.parent_id, Some(fixtures::RUN_2));
        assert_eq!(retry_state.title(), "Current title");
        assert_eq!(
            retry_state.spec.labels.get("team"),
            Some(&"core".to_string())
        );
        assert_eq!(
            retry_state.spec.settings.run.metadata.get("env"),
            Some(&"test".to_string())
        );
        assert_eq!(retry_state.spec.graph.name, "retry_source");
        assert_eq!(
            retry_state.spec.graph_source.as_deref(),
            Some("digraph retry_source { start -> exit }")
        );
        assert_eq!(retry_state.spec.git, Some(git_context()));
        assert_eq!(retry_state.spec.manifest_blob, manifest_blob);
        assert_eq!(retry_state.spec.definition_blob, definition_blob);
        assert_eq!(retry_state.spec.fork_source_ref, Some(fork_source_ref));
        assert_eq!(
            retry_state
                .spec
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.subject.as_ref()),
            Some(&actor("retry-user"))
        );
        assert_eq!(
            retry_state.web_url.as_deref(),
            Some("http://localhost:3000/runs/retry")
        );

        assert!(retry_state.checkpoints.is_empty());
        assert!(retry_state.conclusion.is_none());
        assert!(retry_state.pull_request.is_none());
        assert!(retry_state.pending_interviews.is_empty());
        assert!(retry_state.pending_control.is_none());
        assert!(
            retry_state
                .sandbox
                .as_ref()
                .and_then(fabro_types::RunSandbox::instance)
                .is_none()
        );

        let source_store = store.open_run(&source_run_id).await.unwrap();
        assert_eq!(
            source_store.list_events().await.unwrap().len(),
            source_event_count
        );
        assert_eq!(
            source_store.state().await.unwrap().status,
            RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            }
        );
    }

    #[tokio::test]
    async fn retry_rejects_non_retryable_sources() {
        let store = memory_store();

        let succeeded = fixtures::RUN_1;
        let succeeded_store = store.create_run(&succeeded).await.unwrap();
        append_created(&succeeded_store, succeeded, None, None).await;
        append_started(&succeeded_store, succeeded).await;
        event::append_event(&succeeded_store, &succeeded, &Event::WorkflowRunCompleted {
            timing:               RunTiming::wall_only(10),
            artifact_count:       0,
            status:               "succeeded".to_string(),
            reason:               fabro_types::SuccessReason::Completed,
            total_usd_micros:     None,
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        })
        .await
        .unwrap();

        let active = fixtures::RUN_2;
        let active_store = store.create_run(&active).await.unwrap();
        append_created(&active_store, active, None, None).await;
        event::append_event(&active_store, &active, &Event::RunSubmitted {
            definition_blob: None,
        })
        .await
        .unwrap();
        event::append_event(&active_store, &active, &Event::RunRunnable {
            source: RunRunnableSource::StartRequested,
            actor:  None,
        })
        .await
        .unwrap();

        let archived = fixtures::RUN_3;
        let archived_store = store.create_run(&archived).await.unwrap();
        append_created(&archived_store, archived, None, None).await;
        append_failed(&archived_store, archived, FailureReason::WorkflowError).await;
        event::append_event(&archived_store, &archived, &Event::RunArchived {
            actor: None,
        })
        .await
        .unwrap();

        for run_id in [succeeded, active, archived] {
            let err = retry_run(&store, &RetryRunInput {
                source_run_id: run_id,
                new_run_id:    RunId::new(),
                provenance:    None,
                web_url:       None,
            })
            .await
            .unwrap_err();
            assert!(
                matches!(err, Error::Precondition(_)),
                "unexpected error: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn retry_reports_missing_source() {
        let store = memory_store();
        let err = retry_run(&store, &RetryRunInput {
            source_run_id: fixtures::RUN_1,
            new_run_id:    RunId::new(),
            provenance:    None,
            web_url:       None,
        })
        .await
        .unwrap_err();

        assert!(
            matches!(err, Error::RunNotFound(_)),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn dead_status_is_retryable() {
        ensure_retryable(RunStatus::Dead, &fixtures::RUN_1).unwrap();
    }

    #[test]
    fn cancelled_status_is_retryable() {
        ensure_retryable(
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
            &fixtures::RUN_1,
        )
        .unwrap();
    }
}
