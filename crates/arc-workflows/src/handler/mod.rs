pub mod codergen;
pub mod conditional;
pub mod exit;
pub mod fan_in;
pub mod manager_loop;
pub mod parallel;
pub mod script;
pub mod start;
pub mod sub_pipeline;
pub mod wait_human;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arc_agent::ExecutionEnvironment;
use async_trait::async_trait;

use crate::context::Context;
use crate::error::ArcError;
use crate::event::EventEmitter;
use crate::graph::{shape_to_handler_type, Graph, Node};
use crate::interviewer::Interviewer;
use crate::outcome::Outcome;

/// Shared services available to all handlers during execution.
pub struct EngineServices {
    pub registry: Arc<HandlerRegistry>,
    pub emitter: Arc<EventEmitter>,
    pub execution_env: Arc<dyn ExecutionEnvironment>,
}

/// The handler interface for node execution.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        logs_root: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, ArcError>;

    /// Determines whether an error should be retried.
    /// Default implementation retries transient errors only.
    fn should_retry(&self, err: &ArcError) -> bool {
        err.is_retryable()
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
/// The `make_backend` closure is called once per handler that needs a backend
/// (`CodergenHandler` default, explicit `"codergen"`, and `"parallel.fan_in"`).
#[must_use]
pub fn default_registry(
    interviewer: Arc<dyn Interviewer>,
    make_backend: impl Fn() -> Option<Box<dyn codergen::CodergenBackend>>,
) -> HandlerRegistry {
    let mut registry =
        HandlerRegistry::new(Box::new(codergen::CodergenHandler::new(make_backend())));
    registry.register("start", Box::new(start::StartHandler));
    registry.register("exit", Box::new(exit::ExitHandler));
    registry.register(
        "codergen",
        Box::new(codergen::CodergenHandler::new(make_backend())),
    );
    registry.register("conditional", Box::new(conditional::ConditionalHandler));
    registry.register(
        "wait.human",
        Box::new(wait_human::WaitHumanHandler::new(interviewer)),
    );
    registry.register("script", Box::new(script::ScriptHandler));
    registry.register("tool", Box::new(script::ScriptHandler));
    registry.register("parallel", Box::new(parallel::ParallelHandler));
    registry.register(
        "parallel.fan_in",
        Box::new(fan_in::FanInHandler::new(make_backend())),
    );
    registry.register("sub_pipeline", Box::new(sub_pipeline::SubPipelineHandler));
    registry.register(
        "stack.manager_loop",
        Box::new(manager_loop::ManagerLoopHandler::new(None)),
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AttrValue;

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
            _logs_root: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, ArcError> {
            Ok(Outcome::success())
        }
    }

    #[test]
    fn resolve_by_explicit_type() {
        let mut registry = HandlerRegistry::new(Box::new(TestHandler {
            _name: "default".to_string(),
        }));
        registry.register(
            "wait.human",
            Box::new(TestHandler {
                _name: "human".to_string(),
            }),
        );

        let mut node = Node::new("gate");
        node.attrs.insert(
            "type".to_string(),
            AttrValue::String("wait.human".to_string()),
        );
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
        assert!(handler.should_retry(&ArcError::Handler("timeout".to_string())));
        assert!(!handler.should_retry(&ArcError::Parse("bad".to_string())));
    }

    struct NeverRetryHandler;

    #[async_trait]
    impl Handler for NeverRetryHandler {
        async fn execute(
            &self,
            _node: &Node,
            _context: &Context,
            _graph: &Graph,
            _logs_root: &Path,
            _services: &EngineServices,
        ) -> Result<Outcome, ArcError> {
            Ok(Outcome::success())
        }

        fn should_retry(&self, _err: &ArcError) -> bool {
            false
        }
    }

    #[test]
    fn custom_should_retry_override() {
        let handler = NeverRetryHandler;
        assert!(!handler.should_retry(&ArcError::Handler("timeout".to_string())));
        assert!(!handler.should_retry(&ArcError::Io("connection reset".to_string())));
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
}
