pub mod agent;
pub mod command;
pub mod conditional;
pub mod exit;
pub mod fan_in;
pub mod human;
pub mod llm;
pub mod manager_loop;
pub mod parallel;
pub mod prompt;
pub mod start;
pub mod wait;

use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_store::RunStore;

use crate::context::Context;
use crate::error::FabroError;
use crate::event::EventEmitter;
use crate::outcome::{Outcome, OutcomeExt};
use crate::sandbox_git::GitState;
use fabro_graphviz::graph::{Graph, Node, shape_to_handler_type};
use fabro_hooks::{HookContext, HookDecision, HookRunner};
use fabro_interview::Interviewer;

/// Shared services available to all handlers during execution.
pub struct EngineServices {
    pub registry: Arc<HandlerRegistry>,
    pub emitter: Arc<EventEmitter>,
    pub sandbox: Arc<dyn Sandbox>,
    pub run_store: Option<Arc<dyn RunStore>>,
    /// Git state for the current run. Set via `set_git_state` at the start of
    /// `run_via_core` and read by parallel/fan-in handlers.
    pub(crate) git_state: std::sync::RwLock<Option<Arc<GitState>>>,
    /// Hook runner for user-defined lifecycle hooks.
    pub hook_runner: Option<Arc<HookRunner>>,
    /// Environment variables from `[sandbox.env]` config, injected into command nodes.
    pub env: HashMap<String, String>,
    /// When true, handlers should skip real execution and return simulated results.
    pub dry_run: bool,
}

impl EngineServices {
    /// Read the current git state (if any).
    pub fn git_state(&self) -> Option<Arc<GitState>> {
        self.git_state.read().unwrap().clone()
    }

    /// Set the git state for the current run.
    pub fn set_git_state(&self, state: Option<Arc<GitState>>) {
        *self.git_state.write().unwrap() = state;
    }

    /// Run lifecycle hooks and return the merged decision.
    /// Returns `Proceed` if no hook runner is configured.
    pub async fn run_hooks(&self, hook_context: &HookContext) -> HookDecision {
        let Some(ref runner) = self.hook_runner else {
            return HookDecision::Proceed;
        };
        runner.run(hook_context, self.sandbox.clone(), None).await
    }

    /// Test-only default: empty registry, no hooks, local sandbox at cwd.
    #[cfg(test)]
    pub fn test_default() -> Self {
        Self {
            registry: Arc::new(HandlerRegistry::new(Box::new(start::StartHandler))),
            emitter: Arc::new(EventEmitter::new()),
            sandbox: Arc::new(fabro_agent::LocalSandbox::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
            run_store: None,
            git_state: std::sync::RwLock::new(None),
            hook_runner: None,
            env: HashMap::new(),
            dry_run: false,
        }
    }
}

/// The handler interface for node execution.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, FabroError>;

    /// Produce a simulated result for dry-run mode.
    /// Override for handlers that need custom context updates.
    async fn simulate(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        Ok(Outcome::simulated(&node.id))
    }

    /// Determines whether an error should be retried.
    /// Default implementation retries transient errors only.
    fn should_retry(&self, err: &FabroError) -> bool {
        err.is_retryable()
    }
}

/// Extract a human-readable message from a panic payload.
pub(crate) fn format_panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("handler panicked: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("handler panicked: {s}")
    } else {
        "handler panicked".to_string()
    }
}

/// Route to [`Handler::simulate`] when `services.dry_run` is true, otherwise
/// [`Handler::execute`].
pub async fn dispatch_handler(
    handler: &dyn Handler,
    node: &Node,
    context: &Context,
    graph: &Graph,
    run_dir: &Path,
    services: &EngineServices,
) -> Result<Outcome, FabroError> {
    if services.dry_run {
        handler
            .simulate(node, context, graph, run_dir, services)
            .await
    } else {
        handler
            .execute(node, context, graph, run_dir, services)
            .await
    }
}

/// Maps handler type strings to handler implementations.
pub struct HandlerRegistry {
    handlers: HashMap<String, Box<dyn Handler>>,
    default_handler: Box<dyn Handler>,
}

impl HandlerRegistry {
    #[must_use]
    pub fn new(default_handler: Box<dyn Handler>) -> Self {
        Self {
            handlers: HashMap::new(),
            default_handler,
        }
    }

    /// Register a handler for a given type string.
    pub fn register(&mut self, type_string: impl Into<String>, handler: Box<dyn Handler>) {
        self.handlers.insert(type_string.into(), handler);
    }

    /// Resolve which handler should execute for a given node.
    /// Priority: explicit type -> shape-based -> default.
    #[must_use]
    pub fn resolve(&self, node: &Node) -> &dyn Handler {
        // 1. Explicit type attribute
        if let Some(node_type) = node.node_type() {
            if let Some(handler) = self.handlers.get(node_type) {
                return handler.as_ref();
            }
        }

        // 2. Shape-based resolution
        if let Some(handler_type) = shape_to_handler_type(node.shape()) {
            if let Some(handler) = self.handlers.get(handler_type) {
                return handler.as_ref();
            }
        }

        // 3. Default
        self.default_handler.as_ref()
    }
}

/// Build a [`HandlerRegistry`] with all built-in handler types registered.
///
/// The `make_backend` closure is called for each handler that needs a backend
/// (default, `"agent"`, `"agent_loop"`, `"prompt"`, `"one_shot"`, and `"parallel.fan_in"`).
#[must_use]
pub fn default_registry(
    interviewer: Arc<dyn Interviewer>,
    make_backend: impl Fn() -> Option<Box<dyn agent::CodergenBackend>>,
) -> HandlerRegistry {
    let mut registry = HandlerRegistry::new(Box::new(agent::AgentHandler::new(make_backend())));
    registry.register("start", Box::new(start::StartHandler));
    registry.register("exit", Box::new(exit::ExitHandler));
    registry.register("agent", Box::new(agent::AgentHandler::new(make_backend())));
    // Legacy alias
    registry.register(
        "agent_loop",
        Box::new(agent::AgentHandler::new(make_backend())),
    );
    registry.register(
        "prompt",
        Box::new(prompt::PromptHandler::new(make_backend())),
    );
    // Legacy alias
    registry.register(
        "one_shot",
        Box::new(prompt::PromptHandler::new(make_backend())),
    );
    registry.register("conditional", Box::new(conditional::ConditionalHandler));
    registry.register("human", Box::new(human::HumanHandler::new(interviewer)));
    registry.register("command", Box::new(command::CommandHandler));
    registry.register("tool", Box::new(command::CommandHandler));
    registry.register("parallel", Box::new(parallel::ParallelHandler));
    registry.register(
        "parallel.fan_in",
        Box::new(fan_in::FanInHandler::new(make_backend())),
    );
    registry.register(
        "stack.manager_loop",
        Box::new(manager_loop::SubWorkflowHandler),
    );
    registry.register("wait", Box::new(wait::WaitHandler));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::AttrValue;

    struct TestHandler {
        _name: String,
    }

    #[async_trait]
    impl Handler for TestHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, FabroError> {
            Ok(Outcome::success())
        }
    }

    #[test]
    fn resolve_by_explicit_type() {
        let mut registry = HandlerRegistry::new(Box::new(TestHandler {
            _name: "default".to_string(),
        }));
        registry.register(
            "human",
            Box::new(TestHandler {
                _name: "human".to_string(),
            }),
        );

        let mut node = Node::new("gate");
        node.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));
        let handler = registry.resolve(&node);
        // We can verify it returns the right handler by checking it doesn't panic
        // and returns a valid reference
        let _ = handler;
    }

    #[test]
    fn resolve_by_shape() {
        let mut registry = HandlerRegistry::new(Box::new(TestHandler {
            _name: "default".to_string(),
        }));
        registry.register(
            "start",
            Box::new(TestHandler {
                _name: "start".to_string(),
            }),
        );

        let mut node = Node::new("entry");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let handler = registry.resolve(&node);
        let _ = handler;
    }

    #[test]
    fn resolve_falls_back_to_default() {
        let registry = HandlerRegistry::new(Box::new(TestHandler {
            _name: "default".to_string(),
        }));
        let node = Node::new("work");
        let handler = registry.resolve(&node);
        let _ = handler;
    }

    #[test]
    fn default_should_retry_uses_is_retryable() {
        let handler = TestHandler {
            _name: "test".to_string(),
        };
        assert!(handler.should_retry(&FabroError::handler("timeout".to_string())));
        assert!(!handler.should_retry(&FabroError::Parse("bad".to_string())));
    }

    struct NeverRetryHandler;

    #[async_trait]
    impl Handler for NeverRetryHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _run_dir: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, FabroError> {
            Ok(Outcome::success())
        }

        fn should_retry(&self, _err: &FabroError) -> bool {
            false
        }
    }

    #[test]
    fn custom_should_retry_override() {
        let handler = NeverRetryHandler;
        assert!(!handler.should_retry(&FabroError::handler("timeout".to_string())));
        assert!(!handler.should_retry(&FabroError::Io("connection reset".to_string())));
    }

    #[test]
    fn register_replaces_existing() {
        let mut registry = HandlerRegistry::new(Box::new(TestHandler {
            _name: "default".to_string(),
        }));
        registry.register(
            "start",
            Box::new(TestHandler {
                _name: "first".to_string(),
            }),
        );
        registry.register(
            "start",
            Box::new(TestHandler {
                _name: "second".to_string(),
            }),
        );
        // Should not panic
        let mut node = Node::new("s");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        let handler = registry.resolve(&node);
        let _ = handler;
    }

    #[tokio::test]
    async fn dispatch_handler_routes_to_simulate_when_dry_run() {
        let handler = TestHandler {
            _name: "test".to_string(),
        };
        let node = Node::new("my_node");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = std::path::Path::new("/tmp/test");
        let mut services = EngineServices::test_default();
        services.dry_run = true;

        let outcome = dispatch_handler(&handler, &node, &context, &graph, run_dir, &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(outcome.notes.as_deref(), Some("[Simulated] my_node"));
    }

    #[tokio::test]
    async fn dispatch_handler_routes_to_execute_when_not_dry_run() {
        let handler = TestHandler {
            _name: "test".to_string(),
        };
        let node = Node::new("my_node");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = std::path::Path::new("/tmp/test");
        let mut services = EngineServices::test_default();
        services.dry_run = false;

        let outcome = dispatch_handler(&handler, &node, &context, &graph, run_dir, &services)
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        // execute() returns success with no notes
        assert!(outcome.notes.is_none());
    }
}
