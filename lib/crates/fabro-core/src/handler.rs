use async_trait::async_trait;

use crate::context::Context;
use crate::error::Result;
use crate::graph::Graph;
use crate::outcome::Outcome;
use crate::retry::RetryPolicy;

#[async_trait]
pub trait NodeHandler<G: Graph>: Send + Sync {
    async fn execute(&self, node: &G::Node, context: &Context, graph: &G) -> Result<Outcome>;

    fn retry_policy(&self, _node: &G::Node, _graph: &G) -> RetryPolicy {
        RetryPolicy::none()
    }

    fn on_retries_exhausted(&self, _node: &G::Node, _last_outcome: Outcome) -> Outcome {
        Outcome::fail("max retries exceeded")
    }
}
