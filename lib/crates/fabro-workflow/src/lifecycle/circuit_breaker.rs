use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use fabro_core::error::{Error as CoreError, Result as CoreResult};
use fabro_core::lifecycle::{EdgeContext, EdgeDecision, RunLifecycle};
use fabro_core::outcome::NodeResult;
use fabro_core::state::ExecutionState;

use crate::error::{FailureCategory, FailureSignature, FailureSignatureExt};
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::outcome::{BilledModelUsage, OutcomeExt, StageStatus};

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = NodeResult<Option<BilledModelUsage>>;

/// Sub-lifecycle responsible for tracking failure signatures and tripping the
/// circuit breaker when deterministic failure cycles are detected.
pub(crate) struct CircuitBreakerLifecycle {
    loop_failure_signatures: Mutex<HashMap<FailureSignature, usize>>,
    restart_failure_signatures: Mutex<HashMap<FailureSignature, usize>>,
    loop_restart_signature_limit: usize,
}

impl CircuitBreakerLifecycle {
    pub(crate) fn new(loop_restart_signature_limit: usize) -> Self {
        Self {
            loop_failure_signatures: Mutex::new(HashMap::new()),
            restart_failure_signatures: Mutex::new(HashMap::new()),
            loop_restart_signature_limit,
        }
    }

    /// Restore circuit breaker state from a checkpoint (for resume).
    pub(crate) fn restore(
        &self,
        loop_sigs: HashMap<FailureSignature, usize>,
        restart_sigs: HashMap<FailureSignature, usize>,
    ) {
        *self.loop_failure_signatures.lock().unwrap() = loop_sigs;
        *self.restart_failure_signatures.lock().unwrap() = restart_sigs;
    }

    /// Snapshot current state for checkpoint building.
    pub(crate) fn snapshot(
        &self,
    ) -> (
        HashMap<FailureSignature, usize>,
        HashMap<FailureSignature, usize>,
    ) {
        let loop_sigs = self.loop_failure_signatures.lock().unwrap().clone();
        let restart_sigs = self.restart_failure_signatures.lock().unwrap().clone();
        (loop_sigs, restart_sigs)
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for CircuitBreakerLifecycle {
    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        let gv = node.inner();
        let outcome = &result.outcome;

        let outcome_failure_category = if outcome.status == StageStatus::Fail {
            outcome.classified_failure_category()
        } else {
            None
        };

        if let Some(fc) = outcome_failure_category {
            let sig_hint = outcome
                .failure
                .as_ref()
                .and_then(|f| f.signature.as_deref());
            let sig = FailureSignature::new(
                &gv.id,
                fc,
                sig_hint,
                outcome.failure.as_ref().map(|f| f.message.as_str()),
            );
            if fc.is_signature_tracked() {
                let mut sigs = self.loop_failure_signatures.lock().unwrap();
                let count = sigs.entry(sig.clone()).or_insert(0);
                *count += 1;
                let limit = self.loop_restart_signature_limit;
                if *count >= limit {
                    return Err(CoreError::Other(format!(
                        "deterministic failure cycle detected: signature {sig} repeated {count} times (limit {limit})"
                    )));
                }
            }
        }

        Ok(())
    }

    async fn on_edge_selected(
        &self,
        ctx: &EdgeContext<'_, WorkflowGraph>,
        _state: &WfRunState,
    ) -> CoreResult<EdgeDecision> {
        // Only guard loop_restart edges
        let Some(ref edge) = ctx.edge else {
            return Ok(EdgeDecision::Continue);
        };
        if !edge.inner().loop_restart() {
            return Ok(EdgeDecision::Continue);
        }

        let outcome = ctx.outcome;

        // Guard: only TransientInfra failures may trigger loop_restart
        let failure_class = outcome.classified_failure_category();
        if let Some(fc) = failure_class {
            if fc != FailureCategory::TransientInfra {
                return Ok(EdgeDecision::Block(format!(
                    "loop_restart blocked: failure_class={fc} (requires transient_infra), failure_reason={}",
                    outcome.failure_reason().unwrap_or("none"),
                )));
            }
        }

        // Circuit breaker: check restart failure signatures
        if let Some(ref failure) = outcome.failure {
            let sig = FailureSignature::new(
                ctx.from,
                failure.category,
                failure.signature.as_deref(),
                Some(failure.message.as_str()),
            );
            if failure.category.is_signature_tracked() {
                let mut sigs = self.restart_failure_signatures.lock().unwrap();
                let count = sigs.entry(sig.clone()).or_insert(0);
                *count += 1;
                let limit = self.loop_restart_signature_limit;
                if *count >= limit {
                    return Ok(EdgeDecision::Block(format!(
                        "loop_restart circuit breaker: signature {sig} repeated {count} times (limit {limit})"
                    )));
                }
            }
        }

        Ok(EdgeDecision::Continue)
    }
}
