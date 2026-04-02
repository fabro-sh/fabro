use std::path::Path;

use async_trait::async_trait;

use fabro_model::Provider;
use fabro_store::NodeVisitRef;

use crate::context::keys;
use crate::context::{Context, WorkflowContext};
use crate::error::FabroError;
use crate::event::WorkflowRunEvent;
use crate::outcome::Outcome;
use crate::run_dir::{node_dir, visit_from_context};
use fabro_graphviz::graph::{Graph, Node};
use tokio::fs;

use super::agent::{
    CodergenBackend, CodergenResult, expand_variables, extract_status_fields,
    sync_provider_used_to_store, truncate,
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
        run_dir: &Path,
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

        // 2. Write prompt to logs
        let visit = visit_from_context(context);
        let stage_dir = node_dir(run_dir, &node.id, visit);
        fs::create_dir_all(&stage_dir).await?;
        let node_ref = NodeVisitRef {
            node_id: &node.id,
            visit: u32::try_from(visit).unwrap_or(u32::MAX),
        };
        if let Some(ref store) = services.run_store {
            store
                .put_node_prompt(&node_ref, &prompt)
                .await
                .map_err(|err| FabroError::handler(err.to_string()))?;
        } else {
            fs::write(stage_dir.join("prompt.md"), &prompt).await?;
        }

        // 3. Call LLM backend (one_shot)
        let (response_text, stage_usage, backend_files_touched) =
            if let Some(backend) = &self.backend {
                let result = backend
                    .one_shot(node, &prompt, system_prompt.as_deref(), &stage_dir)
                    .await;
                match result {
                    Ok(CodergenResult::Full(outcome)) => {
                        sync_provider_used_to_store(&stage_dir, &node_ref, services).await?;
                        let status_json = serde_json::to_string_pretty(&outcome)
                            .unwrap_or_else(|_| "{}".to_string());
                        fs::write(stage_dir.join("status.json"), &status_json).await?;
                        return Ok(outcome);
                    }
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
            .map(|usage| usage.model.clone())
            .or_else(|| node.model().map(String::from))
            .unwrap_or_default();
        let response_provider = node
            .provider()
            .map(String::from)
            .or_else(|| Some(Provider::default_from_env().as_str().to_string()))
            .unwrap_or_default();

        services.emitter.emit(&WorkflowRunEvent::PromptCompleted {
            node_id: node.id.clone(),
            response: response_text.clone(),
            model: response_model,
            provider: response_provider,
            usage: stage_usage.clone(),
        });

        // 4. Write response to logs
        if let Some(ref store) = services.run_store {
            store
                .put_node_response(&node_ref, &response_text)
                .await
                .map_err(|err| FabroError::handler(err.to_string()))?;
        } else {
            fs::write(stage_dir.join("response.md"), &response_text).await?;
        }
        sync_provider_used_to_store(&stage_dir, &node_ref, services).await?;

        // 5. Build and write status
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

        let status_json =
            serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string());
        fs::write(stage_dir.join("status.json"), &status_json).await?;

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::AttrValue;
    use fabro_store::{InMemoryStore, NodeVisitRef, RunStore, Store};
    use fabro_types::fixtures;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    async fn make_services_with_run_store() -> (EngineServices, Arc<dyn RunStore>) {
        let store = InMemoryStore::default();
        let run_store = store
            .create_run(&fixtures::RUN_1, chrono::Utc::now(), None)
            .await
            .unwrap();
        let services = EngineServices {
            run_store: Some(Arc::clone(&run_store)),
            ..EngineServices::test_default()
        };
        (services, run_store)
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
                _emitter: &Arc<crate::event::EventEmitter>,
                _stage_dir: &Path,
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
                _stage_dir: &Path,
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

        let response_content = std::fs::read_to_string(
            tmp.path()
                .join("nodes")
                .join("classify")
                .join("response.md"),
        )
        .unwrap();
        assert_eq!(response_content, "one-shot response");
    }

    #[tokio::test]
    async fn prompt_handler_persists_provider_used_in_run_store() {
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
                _emitter: &Arc<crate::event::EventEmitter>,
                _stage_dir: &Path,
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
                stage_dir: &Path,
            ) -> Result<CodergenResult, FabroError> {
                std::fs::write(
                    stage_dir.join("provider_used.json"),
                    r#"{"mode":"prompt","provider":"openai","model":"gpt-5.4"}"#,
                )
                .unwrap();
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
        let (services, run_store) = make_services_with_run_store().await;

        handler
            .execute(&node, &context, &graph, tmp.path(), &services)
            .await
            .unwrap();

        let snapshot = run_store
            .get_node(&NodeVisitRef {
                node_id: "classify",
                visit: 1,
            })
            .await
            .unwrap();
        assert_eq!(snapshot.provider_used.unwrap()["mode"], "prompt");
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
            _emitter: &Arc<crate::event::EventEmitter>,
            _stage_dir: &Path,
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
            _stage_dir: &Path,
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
