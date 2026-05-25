use std::sync::Arc;

use async_trait::async_trait;
use fabro_graphviz::graph::Node;
use fabro_types::AgentBackend;

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::acp::AgentAcpBackend;
use super::api::EffectiveRequestControls;
use super::routing;
use crate::error::Error;
use crate::event::Emitter;
use crate::handler::NodeTimeoutPolicy;

/// Routes codergen invocations to API or ACP backends based on node attributes.
pub struct BackendRouter {
    api: Box<dyn CodergenBackend>,
    acp: AgentAcpBackend,
}

impl BackendRouter {
    #[must_use]
    pub fn new(api_backend: Box<dyn CodergenBackend>, acp_backend: AgentAcpBackend) -> Self {
        Self {
            api: api_backend,
            acp: acp_backend,
        }
    }

    fn select_backend(node: &Node) -> Result<AgentBackend, Error> {
        routing::select_run_backend(node)
    }

    fn select_one_shot_backend(node: &Node) -> Result<AgentBackend, Error> {
        routing::select_one_shot_backend(node)
    }
}

#[async_trait]
impl CodergenBackend for BackendRouter {
    async fn run(&self, request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
        match Self::select_backend(request.node)? {
            AgentBackend::Api => self.api.run(request).await,
            AgentBackend::Acp => self.acp.run(request).await,
        }
    }

    async fn one_shot(&self, request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
        match Self::select_one_shot_backend(request.node)? {
            AgentBackend::Api => self.api.one_shot(request).await,
            AgentBackend::Acp => {
                unreachable!("ACP one-shot is rejected by select_one_shot_backend")
            }
        }
    }

    async fn shutdown(&self, emitter: &Arc<Emitter>) {
        self.api.shutdown(emitter).await;
    }

    fn effective_request_controls(&self, node: &Node) -> Result<EffectiveRequestControls, Error> {
        match Self::select_backend(node)? {
            AgentBackend::Api => self.api.effective_request_controls(node),
            AgentBackend::Acp => self.acp.effective_request_controls(node),
        }
    }

    fn node_timeout_policy(&self, node: &Node) -> NodeTimeoutPolicy {
        match Self::select_backend(node) {
            Ok(AgentBackend::Api) => self.api.node_timeout_policy(node),
            Ok(AgentBackend::Acp) => self.acp.node_timeout_policy(node),
            Err(_) => NodeTimeoutPolicy::ExecutorEnforced,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use fabro_agent::{LocalSandbox, Sandbox};
    use fabro_graphviz::graph::{AttrValue, Node};
    use fabro_model::{ReasoningEffort, Speed};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::context::Context;
    use crate::event::{Emitter, StageScope};

    #[test]
    fn router_uses_api_by_default() {
        let node = Node::new("test");

        assert_eq!(
            BackendRouter::select_backend(&node).unwrap(),
            AgentBackend::Api
        );
    }

    #[test]
    fn router_rejects_cli_backend() {
        let mut node = Node::new("test");
        node.attrs
            .insert("backend".to_string(), AttrValue::String("cli".to_string()));

        let err = BackendRouter::select_backend(&node).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Validation error: unsupported agent backend \"cli\"; expected one of: api, acp"
        );
    }

    #[tokio::test]
    async fn router_routes_one_shot_to_api_by_default() {
        let node = Node::new("test");
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(
            tempfile::tempdir().unwrap().path().to_path_buf(),
        ));
        let context = Context::new();
        let router = BackendRouter::new(Box::new(StubBackend), AgentAcpBackend::new());
        let emitter = Arc::new(Emitter::default());
        let stage_scope = StageScope::for_handler(&context, "test");

        let result = router
            .one_shot(OneShotRequest {
                node:          &node,
                prompt:        "prompt",
                system_prompt: None,
                emitter:       &emitter,
                stage_scope:   &stage_scope,
                sandbox:       &sandbox,
                cancel_token:  CancellationToken::new(),
            })
            .await
            .unwrap();

        let CodergenResult::Text { text, .. } = result else {
            panic!("expected text result");
        };
        assert_eq!(text, "api one-shot");
    }

    #[test]
    fn router_delegates_effective_request_controls_to_api_backend() {
        let node = Node::new("test");
        let router = BackendRouter::new(Box::new(StubBackend), AgentAcpBackend::new());

        let controls = router.effective_request_controls(&node).unwrap();
        assert_eq!(controls.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(controls.speed, Some(Speed::Fast));
    }

    struct StubBackend;

    #[async_trait]
    impl CodergenBackend for StubBackend {
        async fn run(&self, _request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
            Ok(CodergenResult::Text {
                text:              "api run".to_string(),
                usage:             None,
                files_touched:     Vec::new(),
                last_file_touched: None,
                timing:            fabro_types::StageTiming::default(),
            })
        }

        async fn one_shot(&self, _request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
            Ok(CodergenResult::Text {
                text:              "api one-shot".to_string(),
                usage:             None,
                files_touched:     Vec::new(),
                last_file_touched: None,
                timing:            fabro_types::StageTiming::default(),
            })
        }

        fn effective_request_controls(
            &self,
            _node: &Node,
        ) -> Result<EffectiveRequestControls, Error> {
            Ok(EffectiveRequestControls {
                reasoning_effort: Some(ReasoningEffort::High),
                speed:            Some(Speed::Fast),
            })
        }
    }
}
