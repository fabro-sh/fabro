use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::condition::evaluate_condition;
use crate::context::Context;
use crate::error::ArcError;
use crate::graph::{Graph, Node};
use crate::outcome::{Outcome, StageStatus};

use super::{EngineServices, Handler};

/// Trait for observing child pipeline state during the manager loop.
#[async_trait]
pub trait ChildObserver: Send + Sync {
    /// Launch the child pipeline. Called before the observation loop when `child_autostart` is true.
    async fn launch_child(
        &self,
        _dotfile: &str,
        _workdir: &str,
        _context: &Context,
    ) -> Result<(), ArcError> {
        Ok(())
    }

    /// Ingest child telemetry into the context.
    async fn observe(&self, context: &Context) -> Result<(), ArcError>;

    /// Optionally steer the child pipeline (e.g., write intervention instructions).
    async fn steer(&self, context: &Context, node: &Node) -> Result<(), ArcError>;
}

/// Orchestrates observe/steer/wait cycles over a child pipeline.
pub struct ManagerLoopHandler {
    observer: Option<Box<dyn ChildObserver>>,
}

impl ManagerLoopHandler {
    #[must_use]
    pub fn new(observer: Option<Box<dyn ChildObserver>>) -> Self {
        Self { observer }
    }
}

/// Parse a duration string like "45s", "200ms", "5m" into a Duration.
/// Falls back to 45 seconds on parse failure.
fn parse_duration_str(s: &str) -> Duration {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        if let Some(ms) = secs.strip_suffix('m') {
            // "ms" suffix
            if let Ok(val) = ms.parse::<u64>() {
                return Duration::from_millis(val);
            }
        } else if let Ok(val) = secs.parse::<u64>() {
            return Duration::from_secs(val);
        }
    }
    if let Some(mins) = s.strip_suffix('m') {
        if let Ok(val) = mins.parse::<u64>() {
            return Duration::from_secs(val * 60);
        }
    }
    Duration::from_secs(45)
}

#[async_trait]
impl Handler for ManagerLoopHandler {
    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        _graph: &Graph,
        _logs_root: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, ArcError> {
        let poll_interval = node
            .attrs
            .get("manager.poll_interval")
            .and_then(super::super::graph::types::AttrValue::as_duration)
            .unwrap_or_else(|| {
                let raw = node
                    .attrs
                    .get("manager.poll_interval")
                    .and_then(|v| v.as_str())
                    .unwrap_or("45s");
                parse_duration_str(raw)
            });

        let max_cycles = node
            .attrs
            .get("manager.max_cycles")
            .and_then(super::super::graph::types::AttrValue::as_i64)
            .unwrap_or(1000);
        let max_cycles = u64::try_from(max_cycles).unwrap_or(1000).max(1);

        let stop_condition = node
            .attrs
            .get("manager.stop_condition")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let actions_str = node
            .attrs
            .get("manager.actions")
            .and_then(|v| v.as_str())
            .unwrap_or("observe,wait");
        let do_observe = actions_str.contains("observe");
        let do_steer = actions_str.contains("steer");
        let do_wait = actions_str.contains("wait");

        // Child autostart: launch child pipeline before the observation loop
        let child_autostart = node
            .attrs
            .get("stack.child_autostart")
            .and_then(|v| v.as_str())
            .unwrap_or("true");
        if child_autostart != "false" {
            if let Some(ref observer) = self.observer {
                let child_dotfile = node
                    .attrs
                    .get("stack.child_dotfile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let child_workdir = node
                    .attrs
                    .get("stack.child_workdir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                observer
                    .launch_child(child_dotfile, child_workdir, context)
                    .await?;
            }
        }

        // Steer cooldown tracking
        let steer_cooldown = node
            .attrs
            .get("manager.steer_cooldown")
            .and_then(|v| v.as_str())
            .map_or(Duration::ZERO, parse_duration_str);
        let mut last_steer_time: Option<Instant> = None;

        // Observation loop
        for cycle in 1..=max_cycles {
            // Observe
            if do_observe {
                if let Some(ref observer) = self.observer {
                    observer.observe(context).await?;
                }
            }

            // Steer (with cooldown)
            if do_steer {
                let cooldown_elapsed = match last_steer_time {
                    Some(t) => t.elapsed() >= steer_cooldown,
                    None => true,
                };
                if cooldown_elapsed {
                    if let Some(ref observer) = self.observer {
                        observer.steer(context, node).await?;
                        last_steer_time = Some(Instant::now());
                    }
                }
            }

            // Check child status from context
            let child_status = context.get_string("context.stack.child.status", "");
            if child_status == "completed" || child_status == "failed" {
                let child_outcome = context.get_string("context.stack.child.outcome", "");
                if child_outcome == "success" {
                    return Ok(Outcome {
                        status: StageStatus::Success,
                        notes: Some(format!("Child completed at cycle {cycle}")),
                        ..Outcome::success()
                    });
                }
                if child_status == "failed" {
                    return Ok(Outcome::fail(format!("Child failed at cycle {cycle}")));
                }
            }

            // Evaluate stop condition
            if !stop_condition.is_empty() {
                let dummy_outcome = Outcome::success();
                if evaluate_condition(stop_condition, &dummy_outcome, context) {
                    return Ok(Outcome {
                        status: StageStatus::Success,
                        notes: Some(format!("Stop condition satisfied at cycle {cycle}")),
                        ..Outcome::success()
                    });
                }
            }

            // Wait
            if do_wait {
                tokio::time::sleep(poll_interval).await;
            }
        }

        Ok(Outcome::fail(format!(
            "Max cycles ({max_cycles}) exceeded for manager loop node: {}",
            node.id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use crate::graph::AttrValue;
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;

    fn make_services() -> EngineServices {
        EngineServices {
            registry: std::sync::Arc::new(HandlerRegistry::new(Box::new(StartHandler))),
            emitter: std::sync::Arc::new(EventEmitter::new()),
            execution_env: std::sync::Arc::new(arc_agent::LocalExecutionEnvironment::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
        }
    }

    #[tokio::test]
    async fn manager_loop_max_cycles_exceeded() {
        let handler = ManagerLoopHandler::new(None);
        let mut node = Node::new("manager");
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(2));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );
        let context = Context::new();
        let graph = Graph::new("test");
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(outcome
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("Max cycles"));
    }

    #[tokio::test]
    async fn manager_loop_child_completed_success() {
        let handler = ManagerLoopHandler::new(None);
        let mut node = Node::new("manager");
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(10));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );
        // Pre-set child status to "completed" and outcome to "success"
        let context = Context::new();
        context.set("context.stack.child.status", serde_json::json!("completed"));
        context.set("context.stack.child.outcome", serde_json::json!("success"));
        let graph = Graph::new("test");
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert!(outcome
            .notes
            .as_deref()
            .unwrap()
            .contains("Child completed"));
    }

    #[tokio::test]
    async fn manager_loop_child_failed() {
        let handler = ManagerLoopHandler::new(None);
        let mut node = Node::new("manager");
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(10));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );
        let context = Context::new();
        context.set("context.stack.child.status", serde_json::json!("failed"));
        let graph = Graph::new("test");
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Fail);
        assert!(outcome
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("Child failed"));
    }

    #[tokio::test]
    async fn manager_loop_stop_condition_satisfied() {
        let handler = ManagerLoopHandler::new(None);
        let mut node = Node::new("manager");
        node.attrs
            .insert("manager.max_cycles".to_string(), AttrValue::Integer(10));
        node.attrs.insert(
            "manager.poll_interval".to_string(),
            AttrValue::Duration(Duration::from_millis(1)),
        );
        node.attrs.insert(
            "manager.stop_condition".to_string(),
            AttrValue::String("context.done=true".to_string()),
        );
        let context = Context::new();
        context.set("done", serde_json::json!("true"));
        let graph = Graph::new("test");
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(&node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, StageStatus::Success);
        assert!(outcome
            .notes
            .as_deref()
            .unwrap()
            .contains("Stop condition satisfied"));
    }

    #[test]
    fn parse_duration_str_seconds() {
        assert_eq!(parse_duration_str("45s"), Duration::from_secs(45));
    }

    #[test]
    fn parse_duration_str_milliseconds() {
        assert_eq!(parse_duration_str("200ms"), Duration::from_millis(200));
    }

    #[test]
    fn parse_duration_str_minutes() {
        assert_eq!(parse_duration_str("5m"), Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_str_invalid_fallback() {
        assert_eq!(parse_duration_str("bad"), Duration::from_secs(45));
    }
}
