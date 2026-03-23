use crate::error::AgentError;
use crate::session::Session;
use crate::tool_registry::RegisteredTool;
use crate::tools::required_str;
use crate::types::{AgentEvent, Turn};
use fabro_llm::types::ToolDefinition;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub type SessionFactory = Arc<dyn Fn() -> Session + Send + Sync>;
pub type SubAgentEventCallback = Arc<dyn Fn(AgentEvent) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub output: String,
    pub success: bool,
    pub turns_used: usize,
}

pub struct SubAgent {
    task: Option<tokio::task::JoinHandle<Result<SubAgentResult, AgentError>>>,
    followup_queue: Arc<Mutex<VecDeque<String>>>,
    cancel_token: CancellationToken,
    depth: usize,
}

pub struct SubAgentManager {
    agents: HashMap<String, SubAgent>,
    max_depth: usize,
    event_callback: Option<SubAgentEventCallback>,
}

impl SubAgentManager {
    #[must_use]
    pub fn new(max_depth: usize) -> Self {
        Self {
            agents: HashMap::new(),
            max_depth,
            event_callback: None,
        }
    }

    pub fn set_event_callback(&mut self, cb: SubAgentEventCallback) {
        self.event_callback = Some(cb);
    }

    fn emit_event(&self, event: AgentEvent) {
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    pub fn spawn(
        &mut self,
        mut session: Session,
        task_prompt: String,
        depth: usize,
    ) -> Result<String, AgentError> {
        if depth >= self.max_depth {
            return Err(AgentError::InvalidState(format!(
                "Maximum subagent depth ({}) reached",
                self.max_depth
            )));
        }

        let agent_id = format!("{:08x}", uuid::Uuid::new_v4().as_fields().0);
        let followup_queue = session.followup_queue_handle();
        let cancel_token = session.cancel_token();

        // Subscribe to child session events and forward them via callback
        if let Some(ref cb) = self.event_callback {
            let mut rx = session.subscribe();
            let cb = cb.clone();
            let fwd_agent_id = agent_id.clone();
            let child_depth = depth + 1;
            tokio::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    // Skip streaming / noise events
                    if matches!(
                        &event.event,
                        AgentEvent::TextDelta { .. }
                            | AgentEvent::AssistantOutputReplace { .. }
                            | AgentEvent::ReasoningDelta { .. }
                            | AgentEvent::ToolCallOutputDelta { .. }
                            | AgentEvent::AssistantTextStart
                            | AgentEvent::SessionStarted
                            | AgentEvent::SessionEnded
                            | AgentEvent::SkillExpanded { .. }
                    ) {
                        continue;
                    }
                    cb(AgentEvent::SubAgentEvent {
                        agent_id: fwd_agent_id.clone(),
                        depth: child_depth,
                        event: Box::new(event.event),
                    });
                }
            });
        }

        let task_prompt_for_spawn = task_prompt.clone();
        let task = tokio::spawn(async move {
            session.initialize().await;
            session.process_input(&task_prompt_for_spawn).await?;
            let turns = session.history().turns();
            let last_text = turns.iter().rev().find_map(|t| match t {
                Turn::Assistant { content, .. } => Some(content.clone()),
                _ => None,
            });
            Ok(SubAgentResult {
                output: last_text.unwrap_or_default(),
                success: true,
                turns_used: turns.len(),
            })
        });

        self.agents.insert(
            agent_id.clone(),
            SubAgent {
                task: Some(task),
                followup_queue,
                cancel_token,
                depth: depth + 1,
            },
        );

        self.emit_event(AgentEvent::SubAgentSpawned {
            agent_id: agent_id.clone(),
            depth: depth + 1,
            task: task_prompt,
        });

        Ok(agent_id)
    }

    pub fn send_input(&self, agent_id: &str, message: &str) -> Result<(), AgentError> {
        let agent = self.agents.get(agent_id).ok_or_else(|| {
            AgentError::InvalidState(format!("No agent found with id: {agent_id}"))
        })?;

        agent
            .followup_queue
            .lock()
            .expect("followup queue lock poisoned")
            .push_back(message.to_string());

        Ok(())
    }

    pub async fn wait(&mut self, agent_id: &str) -> Result<SubAgentResult, AgentError> {
        let mut agent = self.agents.remove(agent_id).ok_or_else(|| {
            AgentError::InvalidState(format!("No agent found with id: {agent_id}"))
        })?;

        let depth = agent.depth;

        match agent.task.take() {
            Some(join_handle) => match join_handle.await {
                Ok(Ok(result)) => {
                    self.emit_event(AgentEvent::SubAgentCompleted {
                        agent_id: agent_id.to_string(),
                        depth,
                        success: result.success,
                        turns_used: result.turns_used,
                    });
                    Ok(result)
                }
                Ok(Err(e)) => {
                    self.emit_event(AgentEvent::SubAgentFailed {
                        agent_id: agent_id.to_string(),
                        depth,
                        error: e.clone(),
                    });
                    Err(e)
                }
                Err(e) => {
                    let error_msg = format!("Agent task panicked: {e}");
                    self.emit_event(AgentEvent::SubAgentFailed {
                        agent_id: agent_id.to_string(),
                        depth,
                        error: AgentError::InvalidState(error_msg.clone()),
                    });
                    Err(AgentError::InvalidState(error_msg))
                }
            },
            None => Err(AgentError::InvalidState(format!(
                "Agent {agent_id} has no running task"
            ))),
        }
    }

    pub fn close(&mut self, agent_id: &str) -> Result<(), AgentError> {
        let agent = self.agents.remove(agent_id).ok_or_else(|| {
            AgentError::InvalidState(format!("No agent found with id: {agent_id}"))
        })?;

        agent.cancel_token.cancel();

        if let Some(join_handle) = agent.task {
            join_handle.abort();
        }

        self.emit_event(AgentEvent::SubAgentClosed {
            agent_id: agent_id.to_string(),
            depth: agent.depth,
        });

        Ok(())
    }

    #[cfg(test)]
    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<&SubAgent> {
        self.agents.get(agent_id)
    }
}

pub fn make_spawn_agent_tool(
    manager: Arc<tokio::sync::Mutex<SubAgentManager>>,
    session_factory: SessionFactory,
    current_depth: usize,
) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "spawn_agent".into(),
            description: "Spawn a subagent to work on a delegated task".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task description for the subagent"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory for the subagent"
                    },
                    "model": {
                        "type": "string",
                        "description": "Model to use for the subagent"
                    },
                    "max_turns": {
                        "type": "integer",
                        "description": "Maximum number of turns for the subagent"
                    }
                },
                "required": ["task"]
            }),
        },
        executor: Arc::new(move |args, _ctx| {
            let manager = manager.clone();
            let session_factory = session_factory.clone();
            Box::pin(async move {
                let task = required_str(&args, "task")?;

                // Extract optional max_turns parameter
                let max_turns = args
                    .get("max_turns")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize);

                // Note: working_dir and model require session factory changes to wire through
                let mut session = session_factory();
                // Default subagent max_turns is 0 (unlimited) per spec (overridable via parameter)
                session.set_max_turns(max_turns.unwrap_or(0));
                let mut mgr = manager.lock().await;
                mgr.spawn(session, task.to_string(), current_depth)
                    .map_err(|e| e.to_string())
            })
        }),
    }
}

pub fn make_send_input_tool(manager: Arc<tokio::sync::Mutex<SubAgentManager>>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "send_input".into(),
            description: "Send a follow-up message to a running subagent".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to send input to"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message to send to the agent"
                    }
                },
                "required": ["agent_id", "message"]
            }),
        },
        executor: Arc::new(move |args, _ctx| {
            let manager = manager.clone();
            Box::pin(async move {
                let agent_id = required_str(&args, "agent_id")?;
                let message = required_str(&args, "message")?;

                let mgr = manager.lock().await;
                mgr.send_input(agent_id, message)
                    .map_err(|e| e.to_string())?;
                Ok(format!("Message sent to agent {agent_id}"))
            })
        }),
    }
}

pub fn make_wait_tool(manager: Arc<tokio::sync::Mutex<SubAgentManager>>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "wait".into(),
            description: "Wait for a subagent to complete and return its result".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to wait for"
                    }
                },
                "required": ["agent_id"]
            }),
        },
        executor: Arc::new(move |args, _ctx| {
            let manager = manager.clone();
            Box::pin(async move {
                let agent_id = required_str(&args, "agent_id")?;

                let mut mgr = manager.lock().await;
                let result = mgr.wait(agent_id).await.map_err(|e| e.to_string())?;
                Ok(format!(
                    "Agent completed (success: {}, turns: {})\n\n{}",
                    result.success, result.turns_used, result.output
                ))
            })
        }),
    }
}

pub fn make_close_agent_tool(manager: Arc<tokio::sync::Mutex<SubAgentManager>>) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name: "close_agent".into(),
            description: "Close a running subagent".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to close"
                    }
                },
                "required": ["agent_id"]
            }),
        },
        executor: Arc::new(move |args, _ctx| {
            let manager = manager.clone();
            Box::pin(async move {
                let agent_id = required_str(&args, "agent_id")?;

                let mut mgr = manager.lock().await;
                mgr.close(agent_id).map_err(|e| e.to_string())?;
                Ok(format!("Agent {agent_id} closed"))
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SessionConfig;
    use crate::test_support::*;
    use fabro_llm::provider::ProviderAdapter;
    use fabro_llm::types::Role;

    // --- Tests ---

    #[test]
    fn manager_creation() {
        let manager = SubAgentManager::new(3);
        assert_eq!(manager.max_depth, 3);
        assert!(manager.agents.is_empty());
    }

    #[tokio::test]
    async fn spawn_creates_agent_and_returns_id() {
        let mut manager = SubAgentManager::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let result = manager.spawn(session, "Do something".into(), 0);
        assert!(result.is_ok());
        let agent_id = result.unwrap();
        assert!(!agent_id.is_empty());
        assert!(manager.get(&agent_id).is_some());
    }

    #[tokio::test]
    async fn spawn_initializes_session_before_processing_input() {
        let mut manager = SubAgentManager::new(3);

        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        let session = Session::new(client, profile, env, SessionConfig::default());

        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        let _ = manager.wait(&agent_id).await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        let system_message = request
            .messages
            .iter()
            .find(|message| message.role == Role::System)
            .expect("subagent request should include system message");

        assert!(
            !system_message.text().trim().is_empty(),
            "subagent system prompt should not be empty"
        );
    }

    #[tokio::test]
    async fn depth_limit_enforced() {
        let mut manager = SubAgentManager::new(2);
        let session = make_session(vec![text_response("Hello")]).await;
        let result = manager.spawn(session, "Do something".into(), 2);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Maximum subagent depth"));
    }

    #[tokio::test]
    async fn close_removes_agent() {
        let mut manager = SubAgentManager::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        assert!(manager.get(&agent_id).is_some());

        let result = manager.close(&agent_id);
        assert!(result.is_ok());
        assert!(manager.get(&agent_id).is_none());
    }

    #[tokio::test]
    async fn send_input_nonexistent_agent_errors() {
        let manager = SubAgentManager::new(3);
        let result = manager.send_input("nonexistent-id", "hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No agent found"));
    }

    #[tokio::test]
    async fn wait_nonexistent_agent_errors() {
        let mut manager = SubAgentManager::new(3);
        let result = manager.wait("nonexistent-id").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No agent found"));
    }

    #[tokio::test]
    async fn wait_returns_result() {
        let mut manager = SubAgentManager::new(3);
        let session = make_session(vec![text_response("Task completed successfully")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();

        let result = manager.wait(&agent_id).await;
        assert!(result.is_ok());
        let agent_result = result.unwrap();
        assert_eq!(agent_result.output, "Task completed successfully");
        assert!(agent_result.success);
        assert!(agent_result.turns_used > 0);
        assert!(manager.get(&agent_id).is_none());
    }

    #[test]
    fn tool_definitions_correct() {
        let manager = Arc::new(tokio::sync::Mutex::new(SubAgentManager::new(3)));
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not be called");
        });

        let spawn_tool = make_spawn_agent_tool(manager.clone(), factory, 0);
        assert_eq!(spawn_tool.definition.name, "spawn_agent");
        assert!(spawn_tool.definition.parameters["properties"]["task"].is_object());
        let spawn_required = spawn_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(spawn_required.contains(&serde_json::json!("task")));

        let send_tool = make_send_input_tool(manager.clone());
        assert_eq!(send_tool.definition.name, "send_input");
        assert!(send_tool.definition.parameters["properties"]["agent_id"].is_object());
        assert!(send_tool.definition.parameters["properties"]["message"].is_object());
        let send_required = send_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(send_required.contains(&serde_json::json!("agent_id")));
        assert!(send_required.contains(&serde_json::json!("message")));

        let wait_tool = make_wait_tool(manager.clone());
        assert_eq!(wait_tool.definition.name, "wait");
        assert!(wait_tool.definition.parameters["properties"]["agent_id"].is_object());
        let wait_required = wait_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(wait_required.contains(&serde_json::json!("agent_id")));

        let close_tool = make_close_agent_tool(manager);
        assert_eq!(close_tool.definition.name, "close_agent");
        assert!(close_tool.definition.parameters["properties"]["agent_id"].is_object());
        let close_required = close_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(close_required.contains(&serde_json::json!("agent_id")));
    }

    fn captured_events() -> (SubAgentEventCallback, Arc<Mutex<Vec<AgentEvent>>>) {
        let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cb: SubAgentEventCallback = Arc::new(move |event| {
            events_clone.lock().unwrap().push(event);
        });
        (cb, events)
    }

    #[tokio::test]
    async fn callback_captures_spawn_event() {
        let (cb, events) = captured_events();
        let mut manager = SubAgentManager::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("Hello")]).await;
        let _agent_id = manager.spawn(session, "test task".into(), 0).unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(
            matches!(&captured[0], AgentEvent::SubAgentSpawned { depth: 1, task, .. } if task == "test task")
        );
    }

    #[tokio::test]
    async fn callback_captures_wait_completed_event() {
        let (cb, events) = captured_events();
        let mut manager = SubAgentManager::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("done")]).await;
        let agent_id = manager.spawn(session, "task".into(), 0).unwrap();
        let _result = manager.wait(&agent_id).await.unwrap();

        let captured = events.lock().unwrap();
        assert!(captured.iter().any(|e| matches!(
            e,
            AgentEvent::SubAgentCompleted {
                success: true,
                depth: 1,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn callback_captures_close_event() {
        let (cb, events) = captured_events();
        let mut manager = SubAgentManager::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "task".into(), 1).unwrap();
        manager.close(&agent_id).unwrap();

        let captured = events.lock().unwrap();
        assert!(captured
            .iter()
            .any(|e| matches!(e, AgentEvent::SubAgentClosed { depth: 2, .. })));
    }

    #[tokio::test]
    async fn callback_forwards_child_events() {
        let (cb, events) = captured_events();
        let mut manager = SubAgentManager::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "task".into(), 0).unwrap();

        // Wait for agent to complete - child events arrive asynchronously
        let _result = manager.wait(&agent_id).await.unwrap();

        // Give the forwarding task a moment to process remaining events
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let captured = events.lock().unwrap();
        let forwarded_count = captured
            .iter()
            .filter(|e| matches!(e, AgentEvent::SubAgentEvent { .. }))
            .count();
        // Child session emits at least UserInput and AssistantMessage (filtered from SessionStarted/SessionEnded/etc)
        assert!(
            forwarded_count > 0,
            "expected at least one forwarded child event, got {forwarded_count}"
        );
    }

    #[test]
    fn no_callback_does_not_panic() {
        // Manager without callback should not panic on emit
        let manager = SubAgentManager::new(3);
        manager.emit_event(AgentEvent::SubAgentClosed {
            agent_id: "x".into(),
            depth: 0,
        });
    }
}
