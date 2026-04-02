use std::sync::Arc;

use fabro_agent::SessionEvent;
use fabro_retro::retro::{Retro, derive_retro};
use fabro_retro::retro_agent::{
    RETRO_DATA_DIR, build_retro_prompt, dry_run_narrative, run_retro_agent,
};

use super::types::{Executed, RetroOptions, Retroed};
use crate::event::WorkflowRunEvent;

pub async fn run_retro(options: &RetroOptions, dry_run: bool) -> Option<Retro> {
    let cp = match options.run_store.get_checkpoint().await {
        Ok(Some(cp)) => cp,
        Err(e) => {
            tracing::warn!(error = %e, "Could not load checkpoint, skipping retro");
            if let Some(ref emitter) = options.emitter {
                emitter.emit(&WorkflowRunEvent::RetroFailed {
                    error: e.to_string(),
                    duration_ms: 0,
                });
            }
            return None;
        }
        Ok(None) => {
            tracing::warn!("Could not load checkpoint, skipping retro");
            if let Some(ref emitter) = options.emitter {
                emitter.emit(&WorkflowRunEvent::RetroFailed {
                    error: "checkpoint not found".to_string(),
                    duration_ms: 0,
                });
            }
            return None;
        }
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

    if let Err(err) = options.run_store.put_retro(&retro).await {
        tracing::warn!(error = %err, "Failed to save initial retro to store");
    }

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
                            event: event.event.clone(),
                            session_id: Some(event.session_id.clone()),
                            parent_session_id: event.parent_session_id.clone(),
                        });
                    }
                })
            });
        run_retro_agent(
            &options.sandbox,
            Some(&*options.run_store),
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
            if let Err(err) = options.run_store.put_retro(&retro).await {
                tracing::warn!(error = %err, "Failed to save retro with narrative to store");
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

    use chrono::Utc;
    use fabro_graphviz::graph::Graph;
    use fabro_store::{InMemoryStore, Store};
    use fabro_types::{RunId, Settings, fixtures};

    use super::*;
    use crate::context::Context;
    use crate::event::EventEmitter;
    use crate::pipeline::types::Executed;
    use crate::records::{Checkpoint, CheckpointExt};
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

    async fn test_run_store(
        run_dir: &std::path::Path,
        checkpoint: &Checkpoint,
    ) -> Arc<dyn fabro_store::RunStore> {
        let inner = InMemoryStore::default()
            .create_run(
                &test_run_id(),
                Utc::now(),
                Some(run_dir.to_string_lossy().as_ref()),
            )
            .await
            .unwrap();
        let run_store: Arc<dyn fabro_store::RunStore> = inner;
        run_store.put_checkpoint(checkpoint).await.unwrap();
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

        let emitter = Arc::new(EventEmitter::default());
        let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let executed = Executed {
            graph: Graph::new("test"),
            outcome: Ok(crate::outcome::Outcome::success()),
            run_options: test_run_options(&run_dir),
            run_store: Arc::clone(&run_store),
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

        assert!(retroed.run_store.get_retro().await.unwrap().is_some());
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
