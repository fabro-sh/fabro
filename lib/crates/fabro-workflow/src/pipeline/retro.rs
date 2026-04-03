use std::sync::Arc;

use fabro_agent::SessionEvent;
use fabro_retro::retro::{Retro, derive_retro};
use fabro_retro::retro_agent::{
    RETRO_DATA_DIR, build_retro_prompt, dry_run_narrative, run_retro_agent,
};

use super::types::{Executed, RetroOptions, Retroed};
use crate::event::WorkflowRunEvent;

pub async fn run_retro(options: &RetroOptions, dry_run: bool) -> Option<Retro> {
    let state = match options.run_store.state().await {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "Could not load run state, skipping retro");
            if let Some(ref emitter) = options.emitter {
                emitter.emit(&WorkflowRunEvent::RetroFailed {
                    error: e.to_string(),
                    duration_ms: 0,
                });
            }
            return None;
        }
    };
    let Some(cp) = state.checkpoint else {
        tracing::warn!("Could not load checkpoint, skipping retro");
        if let Some(ref emitter) = options.emitter {
            emitter.emit(&WorkflowRunEvent::RetroFailed {
                error: "checkpoint not found".to_string(),
                duration_ms: 0,
            });
        }
        return None;
    };

    let completed_stages = crate::build_completed_stages(&cp, options.failed);
    let stage_durations = match options.run_store.list_events().await {
        Ok(events) => crate::extract_stage_durations_from_events(&events),
        Err(err) => {
            tracing::warn!(error = %err, "Could not load events from store, skipping stage durations");
            std::collections::HashMap::default()
        }
    };
    let mut retro = derive_retro(
        options.run_id,
        &options.workflow_name,
        &options.goal,
        completed_stages,
        options.run_duration_ms,
        &stage_durations,
    );

    let retro_start = std::time::Instant::now();
    let retro_prompt = build_retro_prompt(RETRO_DATA_DIR);
    if let Some(ref emitter) = options.emitter {
        emitter.emit(&WorkflowRunEvent::RetroStarted {
            prompt: Some(retro_prompt),
            provider: Some(options.provider.as_str().to_string()),
            model: Some(options.model.clone()),
        });
    }

    let retro_result = if dry_run {
        Ok((dry_run_narrative(), String::new()))
    } else if let Some(client) = options.llm_client.as_ref() {
        let emitter_clone = options.emitter.clone();
        let event_callback: Option<Arc<dyn Fn(SessionEvent) + Send + Sync>> =
            emitter_clone.map(|emitter| -> Arc<dyn Fn(SessionEvent) + Send + Sync> {
                Arc::new(move |event: SessionEvent| {
                    emitter.touch();
                    if !event.event.is_streaming_noise() {
                        emitter.emit(&WorkflowRunEvent::Agent {
                            stage: "retro".to_string(),
                            visit: 1,
                            event: event.event.clone(),
                            session_id: Some(event.session_id.clone()),
                            parent_session_id: event.parent_session_id.clone(),
                        });
                    }
                })
            });
        run_retro_agent(
            &options.sandbox,
            options.run_store.as_ref(),
            &options.run_dir,
            client,
            options.provider,
            &options.model,
            event_callback,
        )
        .await
        .map(|result| (result.narrative, result.response))
    } else {
        Err(anyhow::anyhow!("No LLM client available"))
    };

    let duration_ms = u64::try_from(retro_start.elapsed().as_millis()).unwrap();
    match retro_result {
        Ok((narrative, response)) => {
            retro.apply_narrative(narrative);
            if let Some(ref emitter) = options.emitter {
                emitter.emit(&WorkflowRunEvent::RetroCompleted {
                    duration_ms,
                    response: Some(response),
                    retro: serde_json::to_value(&retro).ok(),
                });
            }
        }
        Err(e) => {
            if let Some(ref emitter) = options.emitter {
                emitter.emit(&WorkflowRunEvent::RetroFailed {
                    error: e.to_string(),
                    duration_ms,
                });
            }
            tracing::debug!(error = %e, "Retro agent skipped");
        }
    }

    Some(retro)
}

/// RETRO phase: generate a retrospective for the workflow run.
///
/// Infallible — errors are logged, not propagated. If disabled, passes through
/// with `retro: None`.
pub async fn retro(executed: Executed, options: &RetroOptions) -> Retroed {
    let Executed {
        graph,
        outcome,
        run_options,
        run_store,
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        final_context: _,
        llm_client: _,
        model: _,
        provider: _,
    } = executed;

    let dry_run = run_options.dry_run_enabled();

    let retro = if options.enabled {
        run_retro(options, dry_run).await
    } else {
        None
    };

    Retroed {
        graph,
        outcome,
        run_options,
        run_store,
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        retro,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chrono::Utc;
    use fabro_graphviz::graph::Graph;
    use fabro_store::SlateStore;
    use fabro_types::{RunId, Settings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::context::Context;
    use crate::event::EventEmitter;
    use crate::event::{StoreProgressLogger, WorkflowRunEvent, append_workflow_event};
    use crate::pipeline::types::Executed;
    use crate::records::{Checkpoint, CheckpointExt, RunRecord};
    use crate::run_options::RunOptions;

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn build_checkpoint() -> Checkpoint {
        let context = Context::new();
        context.set("response.work", serde_json::json!("done"));
        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), crate::outcome::Outcome::success());
        let checkpoint = Checkpoint::from_context(
            &context,
            "work",
            vec!["work".to_string()],
            HashMap::new(),
            outcomes,
            None,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        checkpoint
    }

    fn test_store() -> Arc<SlateStore> {
        Arc::new(SlateStore::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    async fn test_run_store(
        run_dir: &std::path::Path,
        checkpoint: &Checkpoint,
    ) -> fabro_store::RunStoreHandle {
        let created_at = Utc::now();
        let inner = test_store()
            .create_run(
                &test_run_id(),
                created_at,
                Some(run_dir.to_string_lossy().as_ref()),
            )
            .await
            .unwrap();
        let run_store = inner;
        let run_record = RunRecord {
            run_id: test_run_id(),
            created_at,
            settings: Settings::default(),
            graph: Graph::new("test"),
            workflow_slug: None,
            working_directory: run_dir.to_path_buf(),
            host_repo_path: None,
            base_branch: None,
            labels: std::collections::HashMap::new(),
        };
        append_workflow_event(
            run_store.as_ref(),
            &test_run_id(),
            &WorkflowRunEvent::RunCreated {
                run_id: test_run_id(),
                settings: serde_json::to_value(&run_record.settings).unwrap(),
                graph: serde_json::to_value(&run_record.graph).unwrap(),
                workflow_source: None,
                workflow_config: None,
                labels: run_record.labels.clone().into_iter().collect(),
                run_dir: run_dir.to_string_lossy().to_string(),
                working_directory: run_dir.to_string_lossy().to_string(),
                host_repo_path: None,
                base_branch: None,
                workflow_slug: None,
                db_prefix: None,
            },
        )
        .await
        .unwrap();
        append_workflow_event(
            run_store.as_ref(),
            &test_run_id(),
            &WorkflowRunEvent::CheckpointCompleted {
                node_id: checkpoint.current_node.clone(),
                status: "success".to_string(),
                current_node: checkpoint.current_node.clone(),
                completed_nodes: checkpoint.completed_nodes.clone(),
                node_retries: checkpoint.node_retries.clone().into_iter().collect(),
                context_values: checkpoint.context_values.clone().into_iter().collect(),
                node_outcomes: checkpoint.node_outcomes.clone().into_iter().collect(),
                next_node_id: checkpoint.next_node_id.clone(),
                git_commit_sha: checkpoint.git_commit_sha.clone(),
                loop_failure_signatures: checkpoint
                    .loop_failure_signatures
                    .clone()
                    .into_iter()
                    .map(|(signature, count)| (signature.to_string(), count))
                    .collect(),
                restart_failure_signatures: checkpoint
                    .restart_failure_signatures
                    .clone()
                    .into_iter()
                    .map(|(signature, count)| (signature.to_string(), count))
                    .collect(),
                node_visits: checkpoint.node_visits.clone().into_iter().collect(),
                diff: None,
            },
        )
        .await
        .unwrap();
        run_store
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings: Settings::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            run_id: test_run_id(),
            labels: HashMap::new(),
            workflow_slug: None,
            github_app: None,
            host_repo_path: None,
            base_branch: None,
            display_base_sha: None,
            git: None,
        }
    }

    #[tokio::test]
    async fn retro_phase_writes_retro_json() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let checkpoint = build_checkpoint();
        let run_store = test_run_store(&run_dir, &checkpoint).await;

        let emitter = Arc::new(EventEmitter::new(test_run_id()));
        let store_logger = StoreProgressLogger::new(run_store.clone());
        store_logger.register(&emitter);
        let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let executed = Executed {
            graph: Graph::new("test"),
            outcome: Ok(crate::outcome::Outcome::success()),
            run_options: test_run_options(&run_dir),
            run_store: run_store.clone(),
            hook_runner: None,
            emitter: Arc::clone(&emitter),
            sandbox: Arc::clone(&sandbox),
            duration_ms: 1,
            final_context: Context::new(),
            llm_client: None,
            model: "test-model".to_string(),
            provider: fabro_llm::Provider::Anthropic,
        };

        let retroed = retro(
            executed,
            &RetroOptions {
                run_id: test_run_id(),
                run_store,
                workflow_name: "test".to_string(),
                goal: "Ship it".to_string(),
                run_dir: run_dir.clone(),
                sandbox,
                emitter: Some(emitter),
                failed: false,
                run_duration_ms: 1,
                enabled: true,
                llm_client: None,
                provider: fabro_llm::Provider::Anthropic,
                model: "test-model".to_string(),
            },
        )
        .await;
        store_logger.flush().await;

        assert!(retroed.retro.is_some());
    }

    #[tokio::test]
    async fn run_retro_emits_retro_events() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let checkpoint = build_checkpoint();

        let emitter = Arc::new(EventEmitter::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.clone())
        });

        let retro = run_retro(
            &RetroOptions {
                run_id: test_run_id(),
                run_store: test_run_store(&run_dir, &checkpoint).await,
                workflow_name: "test".to_string(),
                goal: "Ship it".to_string(),
                run_dir: run_dir.clone(),
                sandbox: Arc::new(fabro_agent::LocalSandbox::new(
                    std::env::current_dir().unwrap(),
                )),
                emitter: Some(Arc::clone(&emitter)),
                failed: false,
                run_duration_ms: 1,
                enabled: true,
                llm_client: None,
                provider: fabro_llm::Provider::Anthropic,
                model: "test-model".to_string(),
            },
            true,
        )
        .await;

        assert!(retro.is_some());
        let seen = seen.lock().unwrap();
        let retro_started = seen
            .iter()
            .find(|event| event.event == "retro.started")
            .unwrap();
        assert_eq!(retro_started.properties["provider"], "anthropic");
        assert_eq!(retro_started.properties["model"], "test-model");
        assert!(
            retro_started.properties["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.contains("/tmp/retro_data/progress.jsonl"))
        );

        let retro_completed = seen
            .iter()
            .find(|event| event.event == "retro.completed")
            .unwrap();
        assert_eq!(retro_completed.properties["response"], "");
        assert!(retro_completed.properties.get("retro").is_some());
        assert_eq!(retro_completed.properties["retro"]["smoothness"], "smooth");
    }
}
