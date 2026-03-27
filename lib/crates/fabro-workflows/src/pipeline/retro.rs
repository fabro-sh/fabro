use std::sync::Arc;

use fabro_agent::SessionEvent;
use fabro_retro::retro::Retro;

use crate::event::WorkflowRunEvent;
use crate::records::Checkpoint;

use super::types::{Executed, RetroOptions, Retroed};

pub async fn run_retro(options: &RetroOptions, dry_run: bool) -> Option<Retro> {
    let cp = match Checkpoint::load(&options.run_dir.join("checkpoint.json")) {
        Ok(cp) => cp,
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
    };

    let completed_stages = crate::build_completed_stages(&cp, options.failed);
    let stage_durations = fabro_retro::retro::extract_stage_durations(&options.run_dir);
    let mut retro = fabro_retro::retro::derive_retro(
        &options.run_id,
        &options.workflow_name,
        &options.goal,
        completed_stages,
        options.run_duration_ms,
        &stage_durations,
    );

    if let Err(e) = retro.save(&options.run_dir) {
        tracing::warn!(error = %e, "Failed to save initial retro");
    }

    let retro_start = std::time::Instant::now();
    if let Some(ref emitter) = options.emitter {
        emitter.emit(&WorkflowRunEvent::RetroStarted);
    }

    let narrative_result = if dry_run {
        Ok(fabro_retro::retro_agent::dry_run_narrative())
    } else if let Some(client) = options.llm_client.as_ref() {
        let emitter_clone = options.emitter.clone();
        let event_callback: Option<Arc<dyn Fn(SessionEvent) + Send + Sync>> =
            emitter_clone.map(|emitter| -> Arc<dyn Fn(SessionEvent) + Send + Sync> {
                Arc::new(move |event: SessionEvent| {
                    emitter.touch();
                    if !matches!(
                        &event.event,
                        fabro_agent::AgentEvent::SessionStarted
                            | fabro_agent::AgentEvent::SessionEnded
                            | fabro_agent::AgentEvent::AssistantTextStart
                            | fabro_agent::AgentEvent::AssistantOutputReplace { .. }
                            | fabro_agent::AgentEvent::TextDelta { .. }
                            | fabro_agent::AgentEvent::ReasoningDelta { .. }
                            | fabro_agent::AgentEvent::ToolCallOutputDelta { .. }
                            | fabro_agent::AgentEvent::SkillExpanded { .. }
                    ) {
                        emitter.emit(&WorkflowRunEvent::Agent {
                            stage: "retro".to_string(),
                            event: event.event.clone(),
                        });
                    }
                })
            });
        fabro_retro::retro_agent::run_retro_agent(
            &options.sandbox,
            &options.run_dir,
            client,
            options.provider,
            &options.model,
            event_callback,
        )
        .await
    } else {
        Err(anyhow::anyhow!("No LLM client available"))
    };

    let duration_ms = retro_start.elapsed().as_millis() as u64;
    if let Some(ref emitter) = options.emitter {
        match &narrative_result {
            Ok(_) => emitter.emit(&WorkflowRunEvent::RetroCompleted { duration_ms }),
            Err(e) => emitter.emit(&WorkflowRunEvent::RetroFailed {
                error: e.to_string(),
                duration_ms,
            }),
        }
    }

    match narrative_result {
        Ok(narrative) => {
            retro.apply_narrative(narrative);
            if let Err(e) = retro.save(&options.run_dir) {
                tracing::warn!(error = %e, "Failed to save retro with narrative");
            }
        }
        Err(e) => {
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
        hook_runner,
        emitter,
        sandbox,
        duration_ms,
        final_context: _,
        llm_client: _,
        model: _,
        provider: _,
    } = executed;

    let dry_run = run_options.dry_run;

    let retro = if options.enabled {
        run_retro(options, dry_run).await
    } else {
        None
    };

    Retroed {
        graph,
        outcome,
        run_options,
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

    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::Graph;

    use super::*;
    use crate::context::Context;
    use crate::event::{EventEmitter, WorkflowRunEvent};
    use crate::pipeline::types::Executed;
    use crate::records::Checkpoint;
    use crate::run_options::RunOptions;

    fn write_checkpoint(run_dir: &std::path::Path) {
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
        checkpoint.save(&run_dir.join("checkpoint.json")).unwrap();
    }

    fn test_run_options(run_dir: &std::path::Path) -> RunOptions {
        RunOptions {
            config: FabroConfig::default(),
            run_dir: run_dir.to_path_buf(),
            cancel_token: None,
            dry_run: true,
            run_id: "run-test".to_string(),
            labels: HashMap::new(),
            git_author: crate::git::GitAuthor::default(),
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
        write_checkpoint(&run_dir);

        let emitter = Arc::new(EventEmitter::new());
        let sandbox: Arc<dyn fabro_agent::Sandbox> = Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ));
        let executed = Executed {
            graph: Graph::new("test"),
            outcome: Ok(crate::outcome::Outcome::success()),
            run_options: test_run_options(&run_dir),
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
                run_id: "run-test".to_string(),
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

        assert!(run_dir.join("retro.json").exists());
        assert!(retroed.retro.is_some());
    }

    #[tokio::test]
    async fn run_retro_emits_retro_events() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        write_checkpoint(&run_dir);

        let emitter = Arc::new(EventEmitter::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        emitter.on_event({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event.clone())
        });

        let retro = run_retro(
            &RetroOptions {
                run_id: "run-test".to_string(),
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
        assert!(seen
            .iter()
            .any(|event| matches!(event, WorkflowRunEvent::RetroStarted)));
        assert!(seen
            .iter()
            .any(|event| matches!(event, WorkflowRunEvent::RetroCompleted { .. })));
    }
}
