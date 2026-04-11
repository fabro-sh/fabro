use std::path::Path;

use async_trait::async_trait;
use fabro_graphviz::graph::{AttrValue, Graph, Node};
use tokio::time::sleep;

use super::{EngineServices, Handler};
use crate::context::Context;
use crate::error::FabroError;
use crate::outcome::Outcome;

/// Sleeps for a configured duration before proceeding.
pub struct WaitHandler;

#[async_trait]
impl Handler for WaitHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        _graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, FabroError> {
        let duration = node
            .attrs
            .get("duration")
            .and_then(AttrValue::as_duration)
            .ok_or_else(|| {
                FabroError::Validation(format!(
                    "wait node {:?} is missing a valid `duration` attribute",
                    node.id
                ))
            })?;
        sleep(duration).await;
        Ok(Outcome::success())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    #[tokio::test]
    async fn wait_timer_success_with_short_duration() {
        let handler = WaitHandler;
        let mut node = Node::new("wait60");
        node.attrs.insert(
            "duration".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");
        let outcome = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
    }

    #[tokio::test]
    async fn wait_timer_errors_without_duration() {
        let handler = WaitHandler;
        let node = Node::new("wait_no_dur");
        let context = Context::new();
        let graph = Graph::new("test");
        let run_dir = Path::new("/tmp/test");
        let result = handler
            .execute(&node, &context, &graph, run_dir, &make_services())
            .await;
        assert!(result.is_err());
    }
}
