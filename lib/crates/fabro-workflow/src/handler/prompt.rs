use std::path::Path;

use crate::context::keys;
use crate::context::{Context, WorkflowContext};
use crate::error::FabroError;
use crate::event::{Event, StageScope};
use crate::outcome::Outcome;
use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};
use fabro_model::Provider;

use super::agent::{
    CodergenBackend, CodergenResult, expand_variables, extract_status_fields, truncate,
};
use super::{EngineServices, Handler};

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
    async fn simulate(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        Ok(super::agent::simulate_llm_handler(node))
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        _run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        // 1. Build prompt (prepend fidelity preamble if present)
        let raw_prompt = node
            .prompt()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| node.label());
        let expanded = expand_variables(raw_prompt, graph)?;
        let preamble = context.preamble();
        let prompt = if preamble.is_empty() {
            expanded
        } else {
            format!("{preamble}\n\n{expanded}")
        };

        // 1b. Discover project docs for system prompt when project_memory is enabled
        let system_prompt = if node.project_memory() {
            let working_dir = services.sandbox.working_directory();
            let provider = node
                .provider()
                .and_then(|s| s.parse::<Provider>().ok())
                .unwrap_or_else(Provider::default_from_env);
            let docs = fabro_agent::discover_memory(
                &*services.sandbox,
                working_dir,
                working_dir,
                provider,
            )
            .await;

            if docs.is_empty() {
                None
            } else {
                Some(docs.join("\n\n"))
            }
        } else {
            None
        };

        let prompt_provider = node
            .provider()
            .map(String::from)
            .or_else(|| Some(Provider::default_from_env().as_str().to_string()));
        let prompt_model = node.model().map(String::from);
        let stage_scope = StageScope::for_handler(context, &node.id);
        services.emitter.emit_scoped(
            &Event::Prompt {
                stage: node.id.clone(),
                visit: stage_scope.visit,
                text: prompt.clone(),
                mode: Some("prompt".to_string()),
                provider: prompt_provider.clone(),
                model: prompt_model.clone(),
            },
            &stage_scope,
        );

        // 3. Call LLM backend (one_shot)
        let (response_text, stage_usage, backend_files_touched) =
            if let Some(backend) = &self.backend {
                let result = backend
                    .one_shot(node, &prompt, system_prompt.as_deref())
                    .await;
                match result {
                    Ok(CodergenResult::Full(outcome)) => return Ok(outcome),
                    Ok(CodergenResult::Text {
                        text,
                        usage,
                        files_touched,
                        ..
                    }) => (text, usage, files_touched),
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
            .or_else(|| Some(Provider::default_from_env().as_str().to_string()))
            .unwrap_or_default();

        services.emitter.emit_scoped(
            &Event::PromptCompleted {
                node_id: node.id.clone(),
                response: response_text.clone(),
                model: response_model,
                provider: response_provider,
                billing: stage_usage.clone(),
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

        extract_status_fields(&response_text, &mut outcome);
        outcome.usage = stage_usage;
        outcome.files_touched = backend_files_touched;

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::{Database, RunDatabase, StageId};
    use fabro_types::fixtures;
    use object_store::memory::InMemory;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    fn test_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    async fn make_services_with_run_store() -> (
        EngineServices,
        RunDatabase,
        crate::event::StoreProgressLogger,
    ) {
        let store = test_store();
        let run_store = store.create_run(&fixtures::RUN_1).await.unwrap();
        let services = EngineServices {
            emitter: Arc::new(crate::event::Emitter::new(fixtures::RUN_1)),
            run_store: run_store.clone().into(),
            ..EngineServices::test_default()
        };
        let logger = crate::event::StoreProgressLogger::new(run_store.clone());
        logger.register(services.emitter.as_ref());
        (services, run_store, logger)
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
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
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
        use fabro_agent::Sandbox;

        struct OneShotBackend;

        #[async_trait]
        impl CodergenBackend for OneShotBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<crate::event::Emitter>,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                _prompt: &str,
                _system_prompt: Option<&str>,
            ) -> Result<CodergenResult, FabroError> {
                Ok(CodergenResult::Text {
                    text: "one-shot response".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
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
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);

        assert_eq!(
            outcome
                .context_updates
                .get(&crate::context::keys::response_key("classify")),
            Some(&serde_json::json!("one-shot response"))
        );
    }

    #[tokio::test]
    async fn prompt_handler_projects_provider_used_from_prompt_events() {
        use fabro_agent::Sandbox;

        struct ProviderOneShotBackend;

        #[async_trait]
        impl CodergenBackend for ProviderOneShotBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<crate::event::Emitter>,
                _sandbox: &Arc<dyn Sandbox>,
                _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
            ) -> Result<CodergenResult, FabroError> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                _prompt: &str,
                _system_prompt: Option<&str>,
            ) -> Result<CodergenResult, FabroError> {
                Ok(CodergenResult::Text {
                    text: "one-shot response".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                    last_file_touched: None,
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
        let node_state = state.node(&StageId::new("classify", 1)).unwrap();
        assert_eq!(node_state.provider_used.as_ref().unwrap()["mode"], "prompt");
    }

    struct OneShotCapturingBackend {
        captured_prompt: Arc<std::sync::Mutex<Option<String>>>,
        captured_system_prompt: Arc<std::sync::Mutex<Option<Option<String>>>>,
    }

    #[async_trait]
    impl CodergenBackend for OneShotCapturingBackend {
        async fn run(
            &self,
            _node: &Node,
            _prompt: &str,
            _context: &Context,
            _thread_id: Option<&str>,
            _emitter: &Arc<crate::event::Emitter>,
            _sandbox: &Arc<dyn fabro_agent::Sandbox>,
            _tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
        ) -> Result<CodergenResult, FabroError> {
            panic!("run() should not be called for prompt handler");
        }

        async fn one_shot(
            &self,
            _node: &Node,
            prompt: &str,
            system_prompt: Option<&str>,
        ) -> Result<CodergenResult, FabroError> {
            *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
            *self.captured_system_prompt.lock().unwrap() = Some(system_prompt.map(String::from));
            Ok(CodergenResult::Text {
                text: "classified".to_string(),
                usage: None,
                files_touched: Vec::new(),
                last_file_touched: None,
            })
        }
    }

    #[tokio::test]
    async fn prompt_handler_prepends_preamble() {
        use std::sync::Mutex;

        let captured = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt: captured.clone(),
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
            captured_prompt: Arc::new(Mutex::new(None)),
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

        // With project_memory=true (default), one_shot is called (system_prompt captured)
        let sys = captured_sys.lock().unwrap().clone();
        assert!(sys.is_some(), "one_shot should have been called");
    }

    #[tokio::test]
    async fn prompt_handler_passes_none_system_prompt_when_project_memory_false() {
        use std::sync::Mutex;

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = OneShotCapturingBackend {
            captured_prompt: Arc::new(Mutex::new(None)),
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
