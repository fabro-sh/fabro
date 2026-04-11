use std::path::Path;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};

use super::{EngineServices, Handler};
use crate::context::Context;
use crate::error::FabroError;
use crate::outcome::Outcome;

/// No-op handler for pipeline entry point. Returns SUCCESS immediately.
pub struct StartHandler;

#[async_trait]
impl Handler for StartHandler {
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

#[cfg(test)]
mod tests {
    use super::*;
    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    #[tokio::test]
    async fn start_handler_returns_success() {
        let handler = StartHandler;
        let node = Node::new("start");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");
        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
    }
}
