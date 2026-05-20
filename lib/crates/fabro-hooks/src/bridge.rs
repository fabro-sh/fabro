use std::sync::Arc;

use fabro_agent::{Sandbox, ToolHookCallback, ToolHookDecision};
use fabro_types::RunId;

use crate::runner::HookRunner;
use crate::types::{HookContext, HookDecision, HookEvent, HookExecutionContext};

/// Bridge between the workflow hook system and the agent tool-hook callback.
///
/// Created per-node in the workflow engine, capturing the `HookRunner` and
/// context needed to build `HookContext` for tool-level events.
pub struct WorkflowToolHookCallback {
    pub hook_runner:            Arc<HookRunner>,
    pub sandbox:                Arc<dyn Sandbox>,
    pub run_id:                 RunId,
    pub workflow_name:          String,
    pub hook_execution_context: HookExecutionContext,
    pub node_id:                String,
}

impl WorkflowToolHookCallback {
    fn base_context(&self, event: HookEvent, tool_name: &str) -> HookContext {
        let mut ctx = HookContext::new(event, self.run_id, self.workflow_name.clone());
        ctx.node_id = Some(self.node_id.clone());
        ctx.tool_name = Some(tool_name.to_string());
        ctx
    }

    async fn run_hook(&self, ctx: &HookContext) -> HookDecision {
        self.hook_runner
            .run(
                ctx,
                self.sandbox.clone(),
                self.hook_execution_context.clone(),
            )
            .await
    }
}

#[async_trait::async_trait]
impl ToolHookCallback for WorkflowToolHookCallback {
    async fn pre_tool_use(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> ToolHookDecision {
        let mut ctx = self.base_context(HookEvent::PreToolUse, tool_name);
        ctx.tool_input = Some(tool_input.clone());

        match self.run_hook(&ctx).await {
            HookDecision::Block { reason } => ToolHookDecision::Block {
                reason: reason.unwrap_or_else(|| "Blocked by hook".to_string()),
            },
            _ => ToolHookDecision::Proceed,
        }
    }

    async fn post_tool_use(&self, tool_name: &str, tool_call_id: &str, tool_output: &str) {
        let mut ctx = self.base_context(HookEvent::PostToolUse, tool_name);
        ctx.tool_call_id = Some(tool_call_id.to_string());
        ctx.tool_output = Some(tool_output.to_string());

        self.run_hook(&ctx).await;
    }

    async fn post_tool_use_failure(&self, tool_name: &str, tool_call_id: &str, error: &str) {
        let mut ctx = self.base_context(HookEvent::PostToolUseFailure, tool_name);
        ctx.tool_call_id = Some(tool_call_id.to_string());
        ctx.error_message = Some(error.to_string());

        self.run_hook(&ctx).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use fabro_model::Catalog;
    use fabro_types::fixtures;

    use super::*;
    use crate::config::{HookDefinition, HookSettings};
    use crate::executor::HookExecutor;
    use crate::types::{HookContext, HookResult};

    struct CapturingExecutor {
        captured_contexts:           Arc<Mutex<Vec<HookContext>>>,
        captured_execution_contexts: Arc<Mutex<Vec<HookExecutionContext>>>,
        decision:                    HookDecision,
    }

    #[async_trait::async_trait]
    impl HookExecutor for CapturingExecutor {
        async fn execute(
            &self,
            _definition: &HookDefinition,
            context: &HookContext,
            _sandbox: Arc<dyn Sandbox>,
            execution_context: &HookExecutionContext,
            _llm_source: &dyn fabro_auth::CredentialSource,
            _catalog: Arc<Catalog>,
        ) -> HookResult {
            self.captured_contexts.lock().unwrap().push(context.clone());
            self.captured_execution_contexts
                .lock()
                .unwrap()
                .push(execution_context.clone());
            HookResult {
                hook_name:   None,
                decision:    self.decision.clone(),
                duration_ms: 1,
            }
        }
    }

    fn make_hook(event: HookEvent) -> HookDefinition {
        HookDefinition {
            name: Some("test-hook".into()),
            event,
            command: Some("echo test".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: Some(false),
        }
    }

    fn make_sandbox() -> Arc<dyn Sandbox> {
        Arc::new(fabro_agent::LocalSandbox::new(
            std::env::current_dir().unwrap(),
        ))
    }

    fn make_bridge(
        hook_runner: Arc<HookRunner>,
        sandbox: Arc<dyn Sandbox>,
        hook_execution_context: HookExecutionContext,
    ) -> WorkflowToolHookCallback {
        WorkflowToolHookCallback {
            hook_runner,
            sandbox,
            run_id: fixtures::RUN_1,
            workflow_name: "test-wf".into(),
            hook_execution_context,
            node_id: "plan".into(),
        }
    }

    #[tokio::test]
    async fn pre_tool_use_builds_correct_context() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(CapturingExecutor {
            captured_contexts:           captured.clone(),
            captured_execution_contexts: Arc::new(Mutex::new(Vec::new())),
            decision:                    HookDecision::Proceed,
        });
        let config = HookSettings {
            hooks: vec![make_hook(HookEvent::PreToolUse)],
        };
        let runner = Arc::new(HookRunner::with_executor(config, executor));
        let sandbox = make_sandbox();
        let bridge = make_bridge(runner, sandbox, HookExecutionContext::default());

        bridge
            .pre_tool_use("shell", &serde_json::json!({"command": "ls"}))
            .await;

        let contexts = captured.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].event, HookEvent::PreToolUse);
        assert_eq!(contexts[0].tool_name.as_deref(), Some("shell"));
        assert_eq!(
            contexts[0].tool_input,
            Some(serde_json::json!({"command": "ls"}))
        );
        assert_eq!(contexts[0].run_id, fixtures::RUN_1);
        assert_eq!(contexts[0].node_id.as_deref(), Some("plan"));
    }

    #[tokio::test]
    async fn pre_tool_use_maps_block_decision() {
        let executor = Arc::new(CapturingExecutor {
            captured_contexts:           Arc::new(Mutex::new(Vec::new())),
            captured_execution_contexts: Arc::new(Mutex::new(Vec::new())),
            decision:                    HookDecision::Block {
                reason: Some("forbidden".into()),
            },
        });
        let config = HookSettings {
            hooks: vec![make_hook(HookEvent::PreToolUse)],
        };
        let runner = Arc::new(HookRunner::with_executor(config, executor));
        let sandbox = make_sandbox();
        let bridge = make_bridge(runner, sandbox, HookExecutionContext::default());

        let decision = bridge.pre_tool_use("shell", &serde_json::json!({})).await;
        assert_eq!(decision, ToolHookDecision::Block {
            reason: "forbidden".to_string(),
        });
    }

    #[tokio::test]
    async fn pre_tool_use_maps_proceed() {
        let executor = Arc::new(CapturingExecutor {
            captured_contexts:           Arc::new(Mutex::new(Vec::new())),
            captured_execution_contexts: Arc::new(Mutex::new(Vec::new())),
            decision:                    HookDecision::Proceed,
        });
        let config = HookSettings {
            hooks: vec![make_hook(HookEvent::PreToolUse)],
        };
        let runner = Arc::new(HookRunner::with_executor(config, executor));
        let sandbox = make_sandbox();
        let bridge = make_bridge(runner, sandbox, HookExecutionContext::default());

        let decision = bridge.pre_tool_use("shell", &serde_json::json!({})).await;
        assert_eq!(decision, ToolHookDecision::Proceed);
    }

    #[tokio::test]
    async fn post_tool_use_builds_context_with_output() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(CapturingExecutor {
            captured_contexts:           captured.clone(),
            captured_execution_contexts: Arc::new(Mutex::new(Vec::new())),
            decision:                    HookDecision::Proceed,
        });
        let config = HookSettings {
            hooks: vec![make_hook(HookEvent::PostToolUse)],
        };
        let runner = Arc::new(HookRunner::with_executor(config, executor));
        let sandbox = make_sandbox();
        let bridge = make_bridge(runner, sandbox, HookExecutionContext::default());

        bridge
            .post_tool_use("shell", "call_1", "file1.txt\nfile2.txt")
            .await;

        let contexts = captured.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].event, HookEvent::PostToolUse);
        assert_eq!(contexts[0].tool_name.as_deref(), Some("shell"));
        assert_eq!(contexts[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            contexts[0].tool_output.as_deref(),
            Some("file1.txt\nfile2.txt")
        );
    }

    #[tokio::test]
    async fn post_tool_use_failure_builds_context_with_error() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(CapturingExecutor {
            captured_contexts:           captured.clone(),
            captured_execution_contexts: Arc::new(Mutex::new(Vec::new())),
            decision:                    HookDecision::Proceed,
        });
        let config = HookSettings {
            hooks: vec![make_hook(HookEvent::PostToolUseFailure)],
        };
        let runner = Arc::new(HookRunner::with_executor(config, executor));
        let sandbox = make_sandbox();
        let bridge = make_bridge(runner, sandbox, HookExecutionContext::default());

        bridge
            .post_tool_use_failure("shell", "call_1", "command not found")
            .await;

        let contexts = captured.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].event, HookEvent::PostToolUseFailure);
        assert_eq!(contexts[0].tool_name.as_deref(), Some("shell"));
        assert_eq!(contexts[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            contexts[0].error_message.as_deref(),
            Some("command not found")
        );
    }

    #[tokio::test]
    async fn pre_tool_use_passes_supplied_hook_execution_context() {
        let captured_contexts = Arc::new(Mutex::new(Vec::new()));
        let captured_execution_contexts = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(CapturingExecutor {
            captured_contexts,
            captured_execution_contexts: Arc::clone(&captured_execution_contexts),
            decision: HookDecision::Proceed,
        });
        let config = HookSettings {
            hooks: vec![make_hook(HookEvent::PreToolUse)],
        };
        let runner = Arc::new(HookRunner::with_executor(config, executor));
        let sandbox = make_sandbox();
        let hook_execution_context = HookExecutionContext {
            host_source_dir:  Some(PathBuf::from("/host/source")),
            sandbox_work_dir: Some(PathBuf::from("/supplied/sandbox")),
        };
        let bridge = make_bridge(runner, sandbox, hook_execution_context.clone());

        bridge.pre_tool_use("shell", &serde_json::json!({})).await;

        assert_eq!(captured_execution_contexts.lock().unwrap().as_slice(), &[
            hook_execution_context
        ]);
    }
}
