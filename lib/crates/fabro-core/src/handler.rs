use async_trait::async_trait;

use crate::context::Context;
use crate::error::Result;
use crate::graph::Graph;
use crate::outcome::Outcome;
use crate::retry::RetryPolicy;

#[async_trait]
pub trait NodeHandler<G: Graph>: Send + Sync {
    async fn execute(
        &self,
        node: &G::Node,
        context: &Context,
        graph: &G,
    ) -> Result<Outcome<G::Meta>>;

    async fn context_for_edge_selection(&self, context: &Context, _graph: &G) -> Result<Context> {
        Ok(context.clone())
    }

    fn retry_policy(&self, _node: &G::Node, _graph: &G) -> RetryPolicy {
        RetryPolicy::none()
    }

    fn on_retries_exhausted(
        &self,
        _node: &G::Node,
        _last_outcome: Outcome<G::Meta>,
    ) -> Outcome<G::Meta> {
        Outcome::fail("max retries exceeded")
    }
}
