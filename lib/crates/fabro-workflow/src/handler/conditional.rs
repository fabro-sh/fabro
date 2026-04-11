use std::path::Path;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};

use super::{EngineServices, Handler};
use crate::context::Context;
use crate::error::Error;
use crate::outcome::Outcome;

/// Conditional routing handler. Returns SUCCESS with a note; actual routing
/// is handled by the engine's edge selection algorithm.
pub struct ConditionalHandler;

#[async_trait]
impl Handler for ConditionalHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, Error> {
        let mut outcome = Outcome::success();
        outcome.notes = Some(format!("Conditional node evaluated: {}", node.id));
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    #[tokio::test]
    async fn conditional_handler_returns_success_with_note() {
        let handler = ConditionalHandler;
        let node = Node::new("gate");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");
        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(
            outcome.notes.as_deref(),
            Some("Conditional node evaluated: gate")
        );
    }
}
