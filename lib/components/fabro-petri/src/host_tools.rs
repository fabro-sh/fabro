//! Fabro's run tools inside a Petri run: the adapter from Petri's host tool
//! capability to `register_fabro_run_tools` (integration plan item F3.4).
//!
//! Petri's native agent step asks the [`HostTools`] capability for the
//! host's tools once per agent session, with a [`HostToolContext`] naming
//! the stage the session serves: the run key, the invocation and execution,
//! the node, the firing and the attempt. This module answers with the same
//! tools the legacy worker registers on a stage's Pebble builder,
//! `fabro_run_create`, `fabro_run_get` and the rest, built by
//! `register_fabro_run_tools` over the same [`FabroRunToolServices`]: the
//! worker's authenticated client and the run id every child run is parented
//! to. `fabro exec` and Ask Fabro sessions keep registering the tools on
//! their builders directly; this adapter is only for a run Petri executes.
//!
//! From there Petri treats the tools as any other: the model sees their
//! definitions beside Pebble's, every call passes through the run's tool
//! hooks (a `pre_tool_use` hook from `[[run.hooks]]` can block one), Pebble
//! reports the call on its event stream, and Petri records it under the
//! stage. A sub-agent inherits them through Pebble's own rule, since the
//! registration marks every run tool `allow_in_subagents`.
//!
//! # Identity
//!
//! The run tools need one identity: the Fabro run id, which is Petri's run
//! key for the run (`RunRequest::run_id`) and `FabroRunToolServices::
//! current_run_id`. It is the parent link of every child run a stage
//! creates. No run tool records a stage on the effects it creates, so
//! nothing here derives Fabro's old `StageId` (`node@visit`); the stage a
//! call came from is Petri's own record of the call, under the stage key
//! `(run, execution, firing)`, and this adapter logs that key with the node
//! and attempt when it builds a session's tools.
//!
//! The builder refuses a context whose run key is not the run the services
//! were built for: the tools would parent child runs to the wrong run. That
//! cannot happen in the worker, which builds both from one run id, so it is
//! logged as an error and the session gets no run tools rather than the
//! wrong ones.

use fabro_workflow::run_tools::register_fabro_run_tools;
use fabro_workflow::services::FabroRunToolServices;
use pebble_coding_agent::tools::RegisteredTool;
use petri_attractor_steps::host_tools::{HostToolContext, HostTools};
use tracing::{debug, error};

/// The `HostTools` capability that gives every native agent session of the
/// run Fabro's run tools, bound to `services`. Register it on the runtime
/// the run executes with; `RuntimeSpec::run_tools` does.
#[must_use]
pub fn capability(services: FabroRunToolServices) -> HostTools {
    HostTools::new().with(move |context| tools_for_stage(&services, context))
}

/// The run tools for the session `context` names: what
/// `register_fabro_run_tools` builds for the legacy worker, or nothing when
/// the context's run is not the one `services` serves.
#[must_use]
pub fn tools_for_stage(
    services: &FabroRunToolServices,
    context: &HostToolContext,
) -> Vec<RegisteredTool> {
    let run_id = services.current_run_id.to_string();
    if context.run.as_str() != run_id {
        error!(
            run = %context.run,
            services_run_id = %run_id,
            node = %context.node,
            "the Petri run key is not the run the Fabro run tools serve; the session gets no run tools"
        );
        return Vec::new();
    }
    debug!(
        run = %context.run,
        invocation = %context.invocation,
        execution = %context.execution,
        firing = %context.firing,
        node = %context.node,
        attempt = ?context.attempt,
        "registering Fabro's run tools on a Petri agent session"
    );
    register_fabro_run_tools(services)
}

/// What a test reads back from a Petri run's record about the run tools,
/// without depending on the Petri packages itself.
#[cfg(feature = "test-support")]
pub mod recorded {
    use petri_attractor_steps::hooks::REPORT_EVENT;
    use petri_execution::events::{RunEvent, replay_run};
    use petri_execution::{Access, RunKey, RunStore};
    pub use petri_execution::{ExecutionId, InvocationId};
    use serde_json::Value;

    /// One completed tool call as Petri recorded it: the stage it was
    /// recorded under and Pebble's completion payload.
    #[derive(Clone, Debug)]
    pub struct ToolCall {
        /// The node's instance name.
        pub node:           String,
        pub invocation:     Option<InvocationId>,
        pub execution:      Option<ExecutionId>,
        /// The parent session of a sub-agent's call; `None` for a call of
        /// the stage's own session.
        pub parent_session: Option<String>,
        /// Pebble's `ToolCallCompleted` payload (`tool_name`, `is_error`,
        /// `error_kind`, the output).
        pub payload:        Value,
    }

    /// Every event of the run, replayed from its record.
    async fn events(store: &dyn RunStore, run_id: &str) -> anyhow::Result<Vec<RunEvent>> {
        let logs = store
            .open(&RunKey::new(run_id), Access::Read)
            .await
            .map_err(anyhow::Error::new)?;
        replay_run(&*logs).await.map_err(anyhow::Error::new)
    }

    /// Every completed call of `tool` in the run's record, in record order.
    pub async fn tool_calls(
        store: &dyn RunStore,
        run_id: &str,
        tool: &str,
    ) -> anyhow::Result<Vec<ToolCall>> {
        Ok(events(store, run_id)
            .await?
            .iter()
            .filter_map(|event| {
                let custom = event.custom()?;
                if custom["kind"] != "pebble" {
                    return None;
                }
                let envelope = custom.get("event")?;
                let payload = envelope["event"].get("ToolCallCompleted")?;
                if payload["tool_name"] != tool {
                    return None;
                }
                Some(ToolCall {
                    node:           event
                        .subject
                        .as_ref()
                        .map(|subject| subject.node.name.to_string())
                        .unwrap_or_default(),
                    invocation:     event.context.invocation,
                    execution:      event.context.execution,
                    parent_session: envelope["parent_session_id"].as_str().map(str::to_owned),
                    payload:        payload.clone(),
                })
            })
            .collect())
    }

    /// Every hook report for `event` (`pre_tool_use`, say) in the run's
    /// record, as Petri's hook service recorded it.
    pub async fn hook_reports(
        store: &dyn RunStore,
        run_id: &str,
        event: &str,
    ) -> anyhow::Result<Vec<Value>> {
        Ok(events(store, run_id)
            .await?
            .iter()
            .filter_map(RunEvent::custom)
            .filter(|value| value["kind"] == REPORT_EVENT && value["event"] == event)
            .cloned()
            .collect())
    }
}
