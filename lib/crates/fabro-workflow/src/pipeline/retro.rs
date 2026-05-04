use std::sync::Arc;

use fabro_agent::SessionEvent;
use fabro_dump::BlobReader;
use fabro_llm::client::Client;
use fabro_retro::retro::{Retro, derive_retro};
use fabro_retro::retro_agent::{
    RETRO_DATA_DIR, build_retro_prompt, dry_run_narrative, run_retro_agent,
};

use super::types::{Executed, RetroOptions, Retroed};
use crate::event::Event;

fn exec_output_tail_from_anyhow(err: &anyhow::Error) -> Option<fabro_types::ExecOutputTail> {
    err.chain()
        .find_map(fabro_sandbox::default_redacted_output_tail)
}

pub async fn run_retro(options: &RetroOptions, dry_run: bool) -> Option<Retro> {
    let services = &options.services;
    let state = match services.run_store.state().await {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "Could not load run state, skipping retro");
            services.emitter.emit(&Event::RetroFailed {
                error:            e.to_string(),
                duration_ms:      0,
                exec_output_tail: exec_output_tail_from_anyhow(&e),
            });
            return None;
        }
    };
    let Some(ref cp) = state.checkpoint else {
        tracing::warn!("Could not load checkpoint, skipping retro");
        services.emitter.emit(&Event::RetroFailed {
            error:            "checkpoint not found".to_string(),
            duration_ms:      0,
            exec_output_tail: None,
        });
        return None;
    };

    let completed_stages = crate::build_completed_stages(cp, options.failed);
    let events = match services.run_store.list_events().await {
        Ok(events) => events,
        Err(err) => {
            tracing::warn!(error = %err, "Could not load events from store, skipping retro");
            services.emitter.emit(&Event::RetroFailed {
                error:            err.to_string(),
                duration_ms:      0,
                exec_output_tail: exec_output_tail_from_anyhow(&err),
            });
            return None;
        }
    };
    let stage_durations = crate::extract_stage_durations_from_events(&events);
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
    services.emitter.emit(&Event::RetroStarted {
        prompt:   Some(retro_prompt),
        provider: Some(services.provider.to_string()),
        model:    Some(options.model.clone()),
    });

    let retro_result = if dry_run {
        Ok((dry_run_narrative(), String::new()))
    } else {
        match Client::from_source(services.llm_source.as_ref()).await {
            Ok(client) => {
                let emitter = Arc::clone(&services.emitter);
                let event_callback: Arc<dyn Fn(SessionEvent) + Send + Sync> =
                    Arc::new(move |event: SessionEvent| {
                        emitter.touch();
                        if !event.event.is_streaming_noise() {
                            emitter.emit(&Event::Agent {
                                stage:             "retro".to_string(),
                                visit:             1,
                                event:             event.event.clone(),
                                session_id:        Some(event.session_id.clone()),
                                parent_session_id: event.parent_session_id.clone(),
                            });
                        }
                    });
                let run_store = services.run_store.clone();
                let run_log = match services.run_store.read_run_log().await {
                    Ok(log) => log,
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "failed to fetch run.log for retro; continuing without it"
                        );
                        None
                    }
                };
                let blob_reader: BlobReader = Box::new(move |blob_id| {
                    let run_store = run_store.clone();
                    Box::pin(async move { run_store.read_blob(&blob_id).await })
                });
                run_retro_agent(
                    &services.sandbox,
                    &state,
                    &events,
                    run_log,
                    Some(blob_reader),
                    &client,
                    services.provider,
                    &options.model,
                    Some(event_callback),
                )
                .await
                .map(|result| (result.narrative, result.response))
            }
            Err(err) => Err(anyhow::anyhow!(err.to_string())),
        }
    };

    let duration_ms = crate::millis_u64(retro_start.elapsed());
    match retro_result {
        Ok((narrative, response)) => {
            retro.apply_narrative(narrative);
            services.emitter.emit(&Event::RetroCompleted {
                duration_ms,
                response: Some(response),
                retro: serde_json::to_value(&retro).ok(),
            });
        }
        Err(e) => {
            services.emitter.emit(&Event::RetroFailed {
                error: e.to_string(),
                duration_ms,
                exec_output_tail: exec_output_tail_from_anyhow(&e),
            });
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
        duration_ms,
        final_context: _,
        engine,
        model: _,
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
        duration_ms,
        services: Arc::clone(&engine.run),
        retro,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use fabro_auth::{CredentialSource, EnvCredentialSource};
    use fabro_graphviz::graph::Graph;
    use fabro_store::Database;
    use fabro_types::{RunId, WorkflowSettings, fixtures};
    use object_store::memory::InMemory;

    use super::*;
    use crate::context::Context;
    use crate::event::{Emitter, Event, StoreProgressLogger, append_event};
    use crate::pipeline::types::Executed;
    use crate::records::{Checkpoint, CheckpointExt, RunSpec};
    use crate::run_options::RunOptions;
    use crate::services::{EngineServices, RunServices};

    fn test_run_id() -> RunId {
        fixtures::RUN_1
    }

    fn build_checkpoint() -> Checkpoint {
        let context = Context::new();
        context.set("response.work", serde_json::json!("done"));
        let mut outcomes = HashMap::new();
        outcomes.insert("work".to_string(), crate::outcome::Outcome::success());
        Checkpoint::from_context(
            &context,
            "work",
            vec!["work".to_string()],
            HashMap::new(),
            outcomes,
            None,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    async fn test_run_store(
        run_dir: &std::path::Path,
        checkpoint: &Checkpoint,
    ) -> fabro_store::RunDatabase {
        let inner = test_store().create_run(&test_run_id()).await.unwrap();
        let run_store = inner;
        let run_spec = RunSpec {
            run_id:           test_run_id(),
            settings:         WorkflowSettings::default(),
            graph:            Graph::new("test"),
            workflow_slug:    None,
            source_directory: Some(run_dir.to_string_lossy().to_string()),
            git:              None,
            labels:           std::collections::HashMap::new(),
            provenance:       None,
            manifest_blob:    None,
            definition_blob:  None,
            fork_source_ref:  None,
            in_place:         false,
        };
        append_event(&run_store, &test_run_id(), &Event::RunCreated {
            run_id:           test_run_id(),
            settings:         serde_json::to_value(&run_spec.settings).unwrap(),
            graph:            serde_json::to_value(&run_spec.graph).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           run_spec.labels.clone().into_iter().collect(),
            run_dir:          run_dir.to_string_lossy().to_string(),
            source_directory: run_spec.source_directory.clone(),
            workflow_slug:    None,
            db_prefix:        None,
            provenance:       run_spec.provenance.clone(),
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        })
        .await
        .unwrap();
        append_event(&run_store, &test_run_id(), &Event::CheckpointCompleted {
            node_id: checkpoint.current_node.clone(),
            status: "succeeded".to_string(),
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
        })
        .await
        .unwrap();
        run_store
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            settings:         WorkflowSettings::default(),
            run_dir:          run_dir.to_path_buf(),
            cancel_token:     tokio_util::sync::CancellationToken::new(),
            run_id:           test_run_id(),
            labels:           HashMap::new(),
            workflow_slug:    None,
            github_app:       None,
            pre_run_git:      None,
            fork_source_ref:  None,
            base_branch:      None,
            display_base_sha: None,
            git:              None,
        }
    }

    fn test_llm_source() -> Arc<dyn CredentialSource> {
        Arc::new(EnvCredentialSource::new())
    }

    #[tokio::test]
    async fn retro_phase_persists_retro_in_projection() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let checkpoint = build_checkpoint();
        let run_store = test_run_store(&run_dir, &checkpoint).await;

        let emitter = Arc::new(Emitter::new(test_run_id()));
        let store_logger = StoreProgressLogger::new(run_store.clone());
        store_logger.register(&emitter);
        let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let services = RunServices::new(
            run_store.clone().into(),
            Arc::clone(&emitter),
            Arc::clone(&sandbox),
            None,
            tokio_util::sync::CancellationToken::new(),
            fabro_llm::Provider::Anthropic,
            test_llm_source(),
            Arc::new(crate::sandbox_git_runtime::SandboxGitRuntime::new()),
            Arc::new(crate::run_metadata::RunMetadataRuntime::new()),
            None,
        );
        let mut engine = EngineServices::test_default();
        engine.run = Arc::clone(&services);
        let executed = Executed {
            graph:         Graph::new("test"),
            outcome:       Ok(crate::outcome::Outcome::success()),
            run_options:   test_run_options(&run_dir),
            duration_ms:   1,
            final_context: Context::new(),
            engine:        Arc::new(engine),
            model:         "test-model".to_string(),
        };

        let retroed = retro(executed, &RetroOptions {
            run_id: test_run_id(),
            services,
            workflow_name: "test".to_string(),
            goal: "Ship it".to_string(),
            failed: false,
            run_duration_ms: 1,
            enabled: true,
            model: "test-model".to_string(),
        })
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

        let emitter = Arc::new(Emitter::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.clone())
        });
        let services = RunServices::new(
            test_run_store(&run_dir, &checkpoint).await.into(),
            Arc::clone(&emitter),
            Arc::new(fabro_agent::LocalSandbox::new(
                std::env::current_dir().unwrap(),
            )),
            None,
            tokio_util::sync::CancellationToken::new(),
            fabro_llm::Provider::Anthropic,
            test_llm_source(),
            Arc::new(crate::sandbox_git_runtime::SandboxGitRuntime::new()),
            Arc::new(crate::run_metadata::RunMetadataRuntime::new()),
            None,
        );

        let retro = run_retro(
            &RetroOptions {
                run_id: test_run_id(),
                services,
                workflow_name: "test".to_string(),
                goal: "Ship it".to_string(),
                failed: false,
                run_duration_ms: 1,
                enabled: true,
                model: "test-model".to_string(),
            },
            true,
        )
        .await;

        assert!(retro.is_some());
        let seen = seen.lock().unwrap();
        let retro_started = seen
            .iter()
            .find(|event| event.event_name() == "retro.started")
            .unwrap();
        let retro_started_properties = retro_started.properties().unwrap();
        assert_eq!(retro_started_properties["provider"], "anthropic");
        assert_eq!(retro_started_properties["model"], "test-model");
        assert!(
            retro_started_properties["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.contains("/tmp/retro_data/events.jsonl"))
        );

        let retro_completed = seen
            .iter()
            .find(|event| event.event_name() == "retro.completed")
            .unwrap();
        let retro_completed_properties = retro_completed.properties().unwrap();
        assert_eq!(retro_completed_properties["response"], "");
        assert!(retro_completed_properties.get("retro").is_some());
        assert_eq!(retro_completed_properties["retro"]["smoothness"], "smooth");
    }
}
