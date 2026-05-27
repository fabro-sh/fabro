use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};
use fabro_types::{StageModelUsage, StageTiming};

use super::agent::{
    CodergenBackend, CodergenResult, OneShotRequest, emit_stage_prompt, extract_status_fields,
    truncate,
};
use super::llm::routing;
use super::{EngineServices, Handler, structured_output};
use crate::context::{Context, WorkflowContext, keys};
use crate::error::Error;
use crate::event::{Emitter, Event};
use crate::outcome::Outcome;

/// Handler for single-shot LLM calls (no tools, no agent loop).
pub struct PromptHandler {
    backend: Option<Box<dyn CodergenBackend>>,
}

impl PromptHandler {
    #[must_use]
    pub fn new(backend: Option<Box<dyn CodergenBackend>>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Handler for PromptHandler {
    async fn shutdown(&self, emitter: &Arc<Emitter>) {
        if let Some(backend) = self.backend.as_ref() {
            backend.shutdown(emitter).await;
        }
    }

    async fn simulate(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, Error> {
        Ok(super::agent::simulate_llm_handler(node))
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, Error> {
        // 1. Build prompt (prepend fidelity preamble if present)
        let raw_prompt = node
            .prompt()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| node.label());
        let preamble = context.preamble();
        let prompt = if preamble.is_empty() {
            raw_prompt.to_string()
        } else {
            format!("{preamble}\n\n{raw_prompt}")
        };

        // 1b. Discover project docs for system prompt when project_memory is enabled
        let system_prompt = if node.project_memory() {
            let working_dir = services.run.sandbox.working_directory();
            let profile_kind = routing::resolve_node_provider_context(
                services.run.catalog.as_ref(),
                &services.run.provider_id,
                &services.run.model,
                node,
            )?
            .profile_kind;
            let docs = match fabro_agent::discover_memory(
                &*services.run.sandbox,
                working_dir,
                working_dir,
                profile_kind,
                &services.run.cancel_token(),
            )
            .await
            {
                Ok(docs) => docs,
                Err(fabro_agent::Error::Interrupted(fabro_agent::InterruptReason::Cancelled)) => {
                    return Err(Error::Cancelled);
                }
                Err(_) => Vec::new(),
            };

            if docs.is_empty() {
                None
            } else {
                Some(
                    docs.into_iter()
                        .map(|doc| doc.content)
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                )
            }
        } else {
            None
        };

        let stage_scope = emit_stage_prompt(
            services,
            context,
            node,
            &prompt,
            StageModelUsage::MODE_PROMPT,
            self.backend.as_deref(),
        )?;

        // 3. Call LLM backend (one_shot)
        let (response_text, stage_usage, backend_files_touched, timing) =
            if let Some(backend) = &self.backend {
                let result = backend
                    .one_shot(OneShotRequest {
                        node,
                        prompt: &prompt,
                        system_prompt: system_prompt.as_deref(),
                        emitter: &services.run.emitter,
                        stage_scope: &stage_scope,
                        sandbox: &services.run.sandbox,
                        cancel_token: services.run.cancel_token(),
                    })
                    .await;
                match result {
                    Ok(CodergenResult::Full(outcome)) => return Ok(*outcome),
                    Ok(CodergenResult::Text {
                        text,
                        usage,
                        files_touched,
                        timing,
                        ..
                    }) => (text, usage, files_touched, timing),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e) if e.is_retryable() => {
                        return Err(e);
                    }
                    Err(e) => {
                        return Ok(e.to_fail_outcome());
                    }
                }
            } else {
                (
                    format!("[Simulated] Response for stage: {}", node.id),
                    None,
                    Vec::new(),
                    StageTiming::default(),
                )
            };

        let response_model = stage_usage
            .as_ref()
            .map(|usage| usage.model_id().to_string())
            .or_else(|| node.model().map(String::from))
            .unwrap_or_default();
        let response_provider = node
            .provider()
            .map(String::from)
            .or_else(|| Some(services.run.provider_id.to_string()))
            .unwrap_or_default();

        services.run.emitter.emit_scoped(
            &Event::PromptCompleted {
                node_id:  node.id.clone(),
                response: response_text.clone(),
                model:    response_model,
                provider: response_provider,
                billing:  stage_usage.clone(),
            },
            &stage_scope,
        );

        // 4. Build and write status
        let mut outcome = Outcome::success();
        outcome.notes = Some(format!("Stage completed: {}", node.id));
        outcome
            .context_updates
            .insert(keys::LAST_STAGE.to_string(), serde_json::json!(node.id));
        outcome.context_updates.insert(
            keys::LAST_RESPONSE.to_string(),
            serde_json::json!(truncate(&response_text, 200)),
        );
        outcome.context_updates.insert(
            keys::response_key(&node.id),
            serde_json::json!(&response_text),
        );

        if let Some(schema) = structured_output::parse_node_output_schema(node)? {
            if let Ok(validated) =
                structured_output::validate_response_text(&schema, &response_text)
            {
                structured_output::apply_validated_output(node, &schema, &validated, &mut outcome);
            } else {
                let mut failed =
                    structured_output::exhausted_failure_outcome(node.output_retries());
                failed.timing = Some(timing);
                failed.usage = stage_usage;
                failed.files_touched = backend_files_touched;
                return Ok(failed);
            }
        } else {
            extract_status_fields(&response_text, &mut outcome);
        }
        outcome.usage = stage_usage;
        outcome.files_touched = backend_files_touched;
        outcome.timing = Some(timing);

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fabro_graphviz::graph::AttrValue;
    use fabro_model::{ReasoningEffort, Speed};
    use fabro_store::{Database, RunDatabase, StageId};
    use fabro_types::fixtures;
    use object_store::memory::InMemory;
    use tempfile::TempDir;

    use super::*;
    use crate::event::Emitter;
    use crate::handler::agent::CodergenRunRequest;
    use crate::outcome::OutcomeExt;

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        ))
    }

    async fn make_services_with_run_store() -> (
        EngineServices,
        RunDatabase,
        crate::event::StoreProgressLogger,
    ) {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        seed_created(&run_store).await;
        let mut services = EngineServices::test_default();
        services.run = services
            .run
            .with_emitter(Arc::new(Emitter::new(fixtures::RUN_1)))
            .with_run_store(run_store.clone().into());
        let logger = crate::event::StoreProgressLogger::new(run_store.clone());
        logger.register(services.run.emitter.as_ref());
        (services, run_store, logger)
    }

    async fn seed_created(run_store: &RunDatabase) {
        crate::event::append_event(
            run_store,
            &fixtures::RUN_1,
            &crate::event::Event::RunCreated {
                run_id:           fixtures::RUN_1,
                title:            None,
                settings:         serde_json::to_value(fabro_types::WorkflowSettings::default())
                    .unwrap(),
                graph:            serde_json::to_value(fabro_types::Graph::new("test")).unwrap(),
                workflow_source:  None,
                workflow_config:  None,
                labels:           std::collections::BTreeMap::default(),
                run_dir:          "/tmp".to_string(),
                source_directory: None,
                workflow_slug:    None,
                automation:       None,
                db_prefix:        None,
                provenance:       None,
                manifest_blob:    None,
                git:              None,
                fork_source_ref:  None,
                retried_from:     None,
                parent_id:        None,
                web_url:          None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn prompt_handler_simulate() {
        let handler = PromptHandler::new(None);
        let node = Node::new("classify");
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .simulate(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);
        assert_eq!(outcome.notes.as_deref(), Some("[Simulated] classify"));
        assert_eq!(
            outcome
                .context_updates
                .get(crate::context::keys::LAST_STAGE),
            Some(&serde_json::json!("classify"))
        );
        assert!(
            outcome
                .context_updates
                .contains_key(crate::context::keys::LAST_RESPONSE)
        );
        assert_eq!(
            outcome
                .context_updates
                .get(&crate::context::keys::response_key("classify")),
            Some(&serde_json::json!(
                "[Simulated] Response for stage: classify"
            ))
        );
    }

    #[tokio::test]
    async fn prompt_handler_dispatches_to_backend_one_shot() {
        struct OneShotBackend;

        #[async_trait]
        impl CodergenBackend for OneShotBackend {
            async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _request: OneShotRequest<'_>,
            ) -> Result<CodergenResult, Error> {
                Ok(CodergenResult::Text {
                    text:              "one-shot response".to_string(),
                    usage:             None,
                    files_touched:     Vec::new(),
                    last_file_touched: None,
                    timing:            StageTiming::default(),
                })
            }

            fn effective_request_controls(
                &self,
                _node: &Node,
            ) -> Result<crate::handler::llm::api::EffectiveRequestControls, Error> {
                Ok(crate::handler::llm::api::EffectiveRequestControls {
                    reasoning_effort: Some(ReasoningEffort::High),
                    speed:            Some(Speed::Fast),
                })
            }
        }

        let handler = PromptHandler::new(Some(Box::new(OneShotBackend)));
        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);

        assert_eq!(
            outcome
                .context_updates
                .get(&crate::context::keys::response_key("classify")),
            Some(&serde_json::json!("one-shot response"))
        );
    }

    #[tokio::test]
    async fn prompt_handler_copies_backend_timing_to_outcome() {
        struct TimingBackend;

        #[async_trait]
        impl CodergenBackend for TimingBackend {
            async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _request: OneShotRequest<'_>,
            ) -> Result<CodergenResult, Error> {
                Ok(CodergenResult::Text {
                    text:              "one-shot response".to_string(),
                    usage:             None,
                    files_touched:     Vec::new(),
                    last_file_touched: None,
                    timing:            StageTiming::new(0, 200, 300),
                })
            }
        }

        let handler = PromptHandler::new(Some(Box::new(TimingBackend)));
        let node = Node::new("classify");
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        assert_eq!(outcome.timing, Some(StageTiming::new(0, 200, 300)));
    }

    #[tokio::test]
    async fn prompt_handler_custom_output_schema_updates_output_context_key() {
        struct CustomOutputBackend;

        #[async_trait]
        impl CodergenBackend for CustomOutputBackend {
            async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _request: OneShotRequest<'_>,
            ) -> Result<CodergenResult, Error> {
                Ok(CodergenResult::Text {
                    text:              r#"{"passed": true}"#.to_string(),
                    usage:             None,
                    files_touched:     Vec::new(),
                    last_file_touched: None,
                    timing:            StageTiming::default(),
                })
            }
        }

        let handler = PromptHandler::new(Some(Box::new(CustomOutputBackend)));
        let mut node = Node::new("audit");
        node.attrs.insert(
            "output_schema".to_string(),
            AttrValue::String(
                r#"{"type":"object","required":["passed"],"properties":{"passed":{"type":"boolean"}}}"#
                    .to_string(),
            ),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        assert_eq!(
            outcome.context_updates.get("output.audit"),
            Some(&serde_json::json!({"passed": true})),
        );
    }

    #[tokio::test]
    async fn prompt_handler_routing_output_schema_requires_valid_routing_json() {
        struct BadRoutingBackend;

        #[async_trait]
        impl CodergenBackend for BadRoutingBackend {
            async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _request: OneShotRequest<'_>,
            ) -> Result<CodergenResult, Error> {
                Ok(CodergenResult::Text {
                    text:              r#"{"outcome": 123}"#.to_string(),
                    usage:             None,
                    files_touched:     Vec::new(),
                    last_file_touched: None,
                    timing:            StageTiming::default(),
                })
            }
        }

        let handler = PromptHandler::new(Some(Box::new(BadRoutingBackend)));
        let mut node = Node::new("route");
        node.attrs.insert(
            "output_schema".to_string(),
            AttrValue::String("routing".to_string()),
        );
        node.attrs
            .insert("output_retries".to_string(), AttrValue::Integer(0));
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        let outcome = handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: false,
        });
        assert_eq!(
            outcome.failure_reason(),
            Some("output schema validation failed after 0 repair attempt(s)")
        );
    }

    #[tokio::test]
    async fn prompt_handler_projects_provider_used_from_prompt_events() {
        struct ProviderOneShotBackend;

        #[async_trait]
        impl CodergenBackend for ProviderOneShotBackend {
            async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _request: OneShotRequest<'_>,
            ) -> Result<CodergenResult, Error> {
                Ok(CodergenResult::Text {
                    text:              "one-shot response".to_string(),
                    usage:             None,
                    files_touched:     Vec::new(),
                    last_file_touched: None,
                    timing:            StageTiming::default(),
                })
            }

            fn effective_request_controls(
                &self,
                _node: &Node,
            ) -> Result<crate::handler::llm::api::EffectiveRequestControls, Error> {
                Ok(crate::handler::llm::api::EffectiveRequestControls {
                    reasoning_effort: Some(ReasoningEffort::High),
                    speed:            Some(Speed::Fast),
                })
            }
        }

        let handler = PromptHandler::new(Some(Box::new(ProviderOneShotBackend)));
        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();
        let (services, run_store, logger) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();
        logger.flush().await;

        let state = run_store.state().await.unwrap();
        let node_state = state.stage(&StageId::new("classify", 1)).unwrap();
        let provider_used = node_state.provider_used.as_ref().unwrap();
        assert_eq!(provider_used.mode, StageModelUsage::MODE_PROMPT);
        assert_eq!(provider_used.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(provider_used.speed, Some(Speed::Fast));
    }

    struct OneShotCapturingBackend {
        captured_prompt:        Arc<std::sync::Mutex<Option<String>>>,
        captured_system_prompt: Arc<std::sync::Mutex<Option<Option<String>>>>,
    }

    #[async_trait]
    impl CodergenBackend for OneShotCapturingBackend {
        async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
            panic!("run() should not be called for prompt handler");
        }

        async fn one_shot(&self, request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
            *self.captured_prompt.lock().unwrap() = Some(request.prompt.to_string());
            *self.captured_system_prompt.lock().unwrap() =
                Some(request.system_prompt.map(String::from));
            Ok(CodergenResult::Text {
                text:              "classified".to_string(),
                usage:             None,
                files_touched:     Vec::new(),
                last_file_touched: None,
                timing:            StageTiming::default(),
            })
        }
    }

    #[tokio::test]
    async fn prompt_handler_prepends_preamble() {
        use std::sync::Mutex;

        let captured = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt:        captured.clone(),
            captured_system_prompt: Arc::new(Mutex::new(None)),
        };
        let handler = PromptHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        let context = Context::new();
        context.set(
            keys::CURRENT_PREAMBLE,
            serde_json::json!("Prior output here"),
        );
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let prompt = captured.lock().unwrap().clone().unwrap();
        assert!(
            prompt.starts_with("Prior output here"),
            "one_shot prompt should start with preamble, got: {prompt}"
        );
        assert!(prompt.ends_with("Classify this"));
    }

    #[tokio::test]
    async fn prompt_handler_passes_system_prompt_when_project_memory_enabled() {
        use std::sync::Mutex;

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt:        Arc::new(Mutex::new(None)),
            captured_system_prompt: captured_sys.clone(),
        };
        let handler = PromptHandler::new(Some(Box::new(backend)));

        // project_memory defaults to true; sandbox working_directory points to cwd
        // which likely has no AGENTS.md/CLAUDE.md, so system_prompt should be None
        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        // With project_memory=true (default), one_shot is called (system_prompt
        // captured)
        let sys = captured_sys.lock().unwrap().clone();
        assert!(sys.is_some(), "one_shot should have been called");
    }

    #[tokio::test]
    async fn prompt_handler_project_memory_uses_model_agent_profile_override() {
        use std::sync::Mutex;

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt:        Arc::new(Mutex::new(None)),
            captured_system_prompt: captured_sys.clone(),
        };
        let handler = PromptHandler::new(Some(Box::new(backend)));
        let workspace = TempDir::new().unwrap();
        tokio::fs::write(workspace.path().join("CLAUDE.md"), "anthropic memory")
            .await
            .unwrap();
        let overrides: fabro_model::catalog::LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[models.acme-claude]
provider = "acme"
display_name = "Acme Claude"
family = "claude"
default = true
agent_profile = "anthropic"
aliases = ["ac"]

[models.acme-claude.limits]
context_window = 1000

[models.acme-claude.features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        let catalog =
            Arc::new(fabro_model::Catalog::from_builtin_with_overrides(&overrides).unwrap());
        let mut services = make_services();
        services.run = services
            .run
            .with_sandbox(Arc::new(fabro_agent::LocalSandbox::new(
                workspace.path().to_path_buf(),
            )))
            .with_catalog_context(
                Arc::clone(&catalog),
                fabro_model::ProviderId::new("acme"),
                "acme-claude".to_string(),
            );

        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        node.attrs
            .insert("model".to_string(), AttrValue::String("ac".to_string()));
        let context = Context::new();
        let graph = Graph::new("test");

        handler
            .execute(&node, &context, &graph, workspace.path(), &services)
            .await
            .unwrap();

        let sys = captured_sys.lock().unwrap().clone();
        assert!(
            sys.flatten()
                .is_some_and(|system_prompt| system_prompt.contains("anthropic memory")),
            "project memory should use model-level Anthropic profile and read CLAUDE.md"
        );
    }

    #[tokio::test]
    async fn prompt_handler_project_memory_uses_default_model_profile_for_provider_attr() {
        use std::sync::Mutex;

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt:        Arc::new(Mutex::new(None)),
            captured_system_prompt: captured_sys.clone(),
        };
        let handler = PromptHandler::new(Some(Box::new(backend)));
        let workspace = TempDir::new().unwrap();
        tokio::fs::write(workspace.path().join("CLAUDE.md"), "anthropic memory")
            .await
            .unwrap();
        let overrides: fabro_model::catalog::LlmCatalogSettings = toml::from_str(
            r#"
[providers.acme]
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[models.acme-claude]
provider = "acme"
display_name = "Acme Claude"
family = "claude"
default = true
agent_profile = "anthropic"

[models.acme-claude.limits]
context_window = 1000

[models.acme-claude.features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        let catalog =
            Arc::new(fabro_model::Catalog::from_builtin_with_overrides(&overrides).unwrap());
        let mut services = make_services();
        services.run = services
            .run
            .with_sandbox(Arc::new(fabro_agent::LocalSandbox::new(
                workspace.path().to_path_buf(),
            )))
            .with_catalog_context(
                Arc::clone(&catalog),
                fabro_model::ProviderId::new("acme"),
                "acme-claude".to_string(),
            );

        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("acme".to_string()),
        );
        let context = Context::new();
        let graph = Graph::new("test");

        handler
            .execute(&node, &context, &graph, workspace.path(), &services)
            .await
            .unwrap();

        let sys = captured_sys.lock().unwrap().clone();
        assert!(
            sys.flatten()
                .is_some_and(|system_prompt| system_prompt.contains("anthropic memory")),
            "project memory should use the default model's Anthropic profile when only the matching provider is set"
        );
    }

    #[tokio::test]
    async fn prompt_handler_passes_none_system_prompt_when_project_memory_false() {
        use std::sync::Mutex;

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt:        Arc::new(Mutex::new(None)),
            captured_system_prompt: captured_sys.clone(),
        };
        let handler = PromptHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        node.attrs
            .insert("project_memory".to_string(), AttrValue::Boolean(false));
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let sys = captured_sys.lock().unwrap().clone();
        assert_eq!(
            sys,
            Some(None),
            "system_prompt should be None when project_memory=false"
        );
    }
}
