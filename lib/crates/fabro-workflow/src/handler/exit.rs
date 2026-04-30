use std::path::Path;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};

use super::{EngineServices, Handler};
use crate::context::Context;
use crate::error::Error;
use crate::outcome::Outcome;

/// No-op handler for pipeline exit point. Returns SUCCESS immediately.
pub struct ExitHandler;

#[async_trait]
impl Handler for ExitHandler {
    async fn execute(
        &self,
        _node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, Error> {
        Ok(Outcome::success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    #[tokio::test]
    async fn exit_handler_returns_success() {
        let handler = ExitHandler;
        let node = Node::new("exit");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");
        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);
    }
}
