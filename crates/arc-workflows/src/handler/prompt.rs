use std::path::Path;

use async_trait::async_trait;

use arc_llm::provider::Provider;

use crate::context::keys;
use crate::context::Context;
use crate::error::ArcError;
use crate::graph::{Graph, Node};
use crate::outcome::Outcome;

use super::agent::{
    expand_variables, extract_status_fields, truncate, CodergenBackend, CodergenResult,
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
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        logs_root: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, ArcError> {
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
                .llm_provider()
                .and_then(|s| s.parse::<Provider>().ok())
                .unwrap_or(Provider::Anthropic);
            let docs =
                arc_agent::discover_project_docs(&*services.sandbox, working_dir, working_dir, provider).await;
            tracing::debug!(node = %node.id, doc_count = docs.len(), "Project docs discovered for prompt node");
            if docs.is_empty() {
                None
            } else {
                Some(docs.join("\n\n"))
            }
        } else {
            None
        };

        // 2. Write prompt to logs
        let visit = crate::engine::visit_from_context(context);
        let stage_dir = crate::engine::node_dir(logs_root, &node.id, visit);
        tokio::fs::create_dir_all(&stage_dir).await?;
        tokio::fs::write(stage_dir.join("prompt.md"), &prompt).await?;

        // 3. Call LLM backend (one_shot)
        let (response_text, stage_usage, backend_files_touched) =
            if let Some(backend) = &self.backend {
                let result = backend
                    .one_shot(node, &prompt, system_prompt.as_deref(), &stage_dir)
                    .await;
                match result {
                    Ok(CodergenResult::Full(outcome)) => {
                        let status_json = serde_json::to_string_pretty(&outcome)
                            .unwrap_or_else(|_| "{}".to_string());
                        tokio::fs::write(stage_dir.join("status.json"), &status_json).await?;
                        return Ok(outcome);
                    }
                    Ok(CodergenResult::Text {
                        text,
                        usage,
                        files_touched,
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

        // 4. Write response to logs
        tokio::fs::write(stage_dir.join("response.md"), &response_text).await?;

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
        tokio::fs::write(stage_dir.join("status.json"), &status_json).await?;

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use crate::graph::AttrValue;
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_services() -> EngineServices {
        EngineServices {
            registry: Arc::new(HandlerRegistry::new(Box::new(StartHandler))),
            emitter: Arc::new(EventEmitter::new()),
            sandbox: Arc::new(arc_agent::LocalSandbox::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
            git_state: std::sync::RwLock::new(None),
            hook_runner: None,
            env: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn prompt_handler_simulation_mode() {
        let handler = PromptHandler::new(None);
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
        assert!(response_content.contains("[Simulated]"));
    }

    #[tokio::test]
    async fn prompt_handler_dispatches_to_backend_one_shot() {
        use arc_agent::Sandbox;

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
            ) -> Result<CodergenResult, ArcError> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                _prompt: &str,
                _system_prompt: Option<&str>,
                _stage_dir: &Path,
            ) -> Result<CodergenResult, ArcError> {
                Ok(CodergenResult::Text {
                    text: "one-shot response".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
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
    async fn prompt_handler_prepends_preamble() {
        use std::sync::Mutex;

        use arc_agent::Sandbox;

        struct OneShotCapturingBackend {
            captured_prompt: Arc<Mutex<Option<String>>>,
            captured_system_prompt: Arc<Mutex<Option<Option<String>>>>,
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
                _sandbox: &Arc<dyn Sandbox>,
            ) -> Result<CodergenResult, ArcError> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                prompt: &str,
                system_prompt: Option<&str>,
                _stage_dir: &Path,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                *self.captured_system_prompt.lock().unwrap() =
                    Some(system_prompt.map(String::from));
                Ok(CodergenResult::Text {
                    text: "classified".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

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
        context.set(keys::CURRENT_PREAMBLE, serde_json::json!("Prior output here"));
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

        use arc_agent::Sandbox;

        struct CapturingBackend {
            captured_system_prompt: Arc<Mutex<Option<Option<String>>>>,
        }

        #[async_trait]
        impl CodergenBackend for CapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<crate::event::EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn Sandbox>,
            ) -> Result<CodergenResult, ArcError> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                _prompt: &str,
                system_prompt: Option<&str>,
                _stage_dir: &Path,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_system_prompt.lock().unwrap() =
                    Some(system_prompt.map(String::from));
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = CapturingBackend {
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

        use arc_agent::Sandbox;

        struct CapturingBackend {
            captured_system_prompt: Arc<Mutex<Option<Option<String>>>>,
        }

        #[async_trait]
        impl CodergenBackend for CapturingBackend {
            async fn run(
                &self,
                _node: &Node,
                _prompt: &str,
                _context: &Context,
                _thread_id: Option<&str>,
                _emitter: &Arc<crate::event::EventEmitter>,
                _stage_dir: &Path,
                _sandbox: &Arc<dyn Sandbox>,
            ) -> Result<CodergenResult, ArcError> {
                panic!("run() should not be called for prompt handler");
            }

            async fn one_shot(
                &self,
                _node: &Node,
                _prompt: &str,
                system_prompt: Option<&str>,
                _stage_dir: &Path,
            ) -> Result<CodergenResult, ArcError> {
                *self.captured_system_prompt.lock().unwrap() =
                    Some(system_prompt.map(String::from));
                Ok(CodergenResult::Text {
                    text: "ok".to_string(),
                    usage: None,
                    files_touched: Vec::new(),
                })
            }
        }

        let captured_sys = Arc::new(Mutex::new(None));
        let backend = CapturingBackend {
            captured_system_prompt: captured_sys.clone(),
        };
        let handler = PromptHandler::new(Some(Box::new(backend)));

        let mut node = Node::new("classify");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Classify this".to_string()),
        );
        node.attrs.insert(
            "project_memory".to_string(),
            AttrValue::Boolean(false),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let tmp = TempDir::new().unwrap();

        handler
            .execute(&node, &context, &graph, tmp.path(), &make_services())
            .await
            .unwrap();

        let sys = captured_sys.lock().unwrap().clone();
        assert_eq!(sys, Some(None), "system_prompt should be None when project_memory=false");
    }
}
