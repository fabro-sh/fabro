use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};
use fabro_interview::{Answer, AnswerValue, Interviewer, Question, QuestionOption, QuestionType};
use fabro_types::run_event::InterviewOption;
use ulid::Ulid;

use super::{EngineServices, Handler};
use crate::context::{Context, keys};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::millis_u64;
use crate::outcome::{Outcome, OutcomeExt};

/// A choice derived from an outgoing edge.
struct Choice {
    key: String,
    label: String,
    to: String,
}

/// Parse an accelerator key from a label.
/// Patterns: `[K] Label`, `K) Label`, `K - Label`, or first character.
fn parse_accelerator_key(label: &str) -> String {
    let trimmed = label.trim();

    // Pattern: [K] Label
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.find(']') {
            let key = &trimmed[1..end];
            if !key.is_empty() {
                return key.to_string();
            }
        }
    }

    // Pattern: K) Label
    if let Some(paren_pos) = trimmed.find(')') {
        if paren_pos > 0 && paren_pos <= 3 {
            let key = &trimmed[..paren_pos];
            if key.chars().all(char::is_alphanumeric) {
                return key.to_string();
            }
        }
    }

    // Pattern: K - Label
    if let Some(dash_pos) = trimmed.find(" - ") {
        if dash_pos > 0 && dash_pos <= 3 {
            let key = &trimmed[..dash_pos];
            if key.chars().all(char::is_alphanumeric) {
                return key.to_string();
            }
        }
    }

    // Fallback: first character
    trimmed
        .chars()
        .next()
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// Blocks until a human selects an option derived from outgoing edges.
pub struct HumanHandler {
    interviewer: Arc<dyn Interviewer>,
    emitter: Option<Arc<Emitter>>,
}

impl HumanHandler {
    pub fn new(interviewer: Arc<dyn Interviewer>) -> Self {
        Self {
            interviewer,
            emitter: None,
        }
    }

    #[must_use]
    pub fn with_emitter(mut self, emitter: Arc<Emitter>) -> Self {
        self.emitter = Some(emitter);
        self
    }

    fn emit(&self, default_emitter: &Arc<Emitter>, event: &Event, scope: &StageScope) {
        match &self.emitter {
            Some(emitter) => emitter.emit_scoped(event, scope),
            None => default_emitter.emit_scoped(event, scope),
        }
    }
}

#[async_trait]
impl Handler for HumanHandler {
    async fn simulate(
        &self,
        node: &Node,
        _context: &Context,
        graph: &Graph,
        _run_dir: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, Error> {
        let edges = graph.outgoing_edges(&node.id);
        let first_choice = edges.iter().find(|e| !e.freeform());

        if let Some(edge) = first_choice {
            let label = edge.label().filter(|l| !l.is_empty()).unwrap_or(&edge.to);
            let key = parse_accelerator_key(label);
            let mut outcome = Outcome::simulated(&node.id);
            outcome.preferred_label = Some(label.to_string());
            outcome.suggested_next_ids = vec![edge.to.clone()];
            outcome.context_updates.insert(
                keys::HUMAN_GATE_SELECTED.to_string(),
                serde_json::json!(key),
            );
            outcome
                .context_updates
                .insert(keys::HUMAN_GATE_LABEL.to_string(), serde_json::json!(label));
            Ok(outcome)
        } else if let Some(edge) = edges.first() {
            // Only freeform edges — pick the first one
            let mut outcome = Outcome::simulated(&node.id);
            outcome.suggested_next_ids = vec![edge.to.clone()];
            outcome.context_updates.insert(
                keys::HUMAN_GATE_SELECTED.to_string(),
                serde_json::json!("freeform"),
            );
            outcome.context_updates.insert(
                keys::HUMAN_GATE_LABEL.to_string(),
                serde_json::json!("[Simulated] auto-selected"),
            );
            Ok(outcome)
        } else {
            Ok(Outcome::simulated(&node.id))
        }
    }

    async fn execute(
        &self,
        node: &Node,
        context: &Context,
        graph: &Graph,
        _run_dir: &Path,
        services: &EngineServices,
    ) -> Result<Outcome, Error> {
        // 1. Derive choices from outgoing edges
        let edges = graph.outgoing_edges(&node.id);
        let mut freeform_target: Option<String> = None;
        let mut choices: Vec<Choice> = Vec::new();

        for edge in &edges {
            if edge.freeform() {
                freeform_target = Some(edge.to.clone());
                continue;
            }
            let label = edge.label().filter(|l| !l.is_empty()).unwrap_or(&edge.to);
            let key = parse_accelerator_key(label);
            choices.push(Choice {
                key,
                label: label.to_string(),
                to: edge.to.clone(),
            });
        }

        if choices.is_empty() && freeform_target.is_none() {
            return Ok(Outcome::fail_deterministic(
                "No outgoing edges for human gate",
            ));
        }

        // 2. Build question
        let options: Vec<QuestionOption> = choices
            .iter()
            .map(|c| QuestionOption {
                key: c.key.clone(),
                label: c.label.clone(),
            })
            .collect();

        let question_type = if choices.is_empty() {
            QuestionType::Freeform
        } else {
            QuestionType::MultipleChoice
        };
        let mut question = Question::new(node.label(), question_type);
        question.id = Ulid::new().to_string();
        question.options = options;
        question.allow_freeform = freeform_target.is_some();
        question.stage.clone_from(&node.id);

        // Look up the prior node's full response
        if let Some(serde_json::Value::String(last_node)) = context.get(keys::LAST_STAGE) {
            if let Some(serde_json::Value::String(response)) =
                context.get(&keys::response_key(&last_node))
            {
                let text = response.trim();
                if !text.is_empty() {
                    question.context_display = Some(text.to_owned());
                }
            }
        }

        // 3. Present to interviewer
        let question_text = node.label().to_string();
        let question_id = question.id.clone();
        let stage_scope = StageScope::for_handler(context, &node.id);
        self.emit(
            &services.emitter,
            &Event::InterviewStarted {
                question_id: question_id.clone(),
                question: question_text.clone(),
                stage: node.id.clone(),
                question_type: question.question_type.to_string(),
                options: question
                    .options
                    .iter()
                    .map(|option| InterviewOption {
                        key: option.key.clone(),
                        label: option.label.clone(),
                    })
                    .collect(),
                allow_freeform: question.allow_freeform,
                timeout_seconds: question.timeout_seconds,
                context_display: question.context_display.clone(),
            },
            &stage_scope,
        );
        let interview_start = Instant::now();
        let answer = self.interviewer.ask(question).await;

        // 4. Handle timeout
        if answer.value == AnswerValue::Timeout {
            self.emit(
                &services.emitter,
                &Event::InterviewTimeout {
                    question_id: question_id.clone(),
                    question: question_text,
                    stage: node.id.clone(),
                    duration_ms: millis_u64(interview_start.elapsed()),
                },
                &stage_scope,
            );
            let default_choice = node
                .attrs
                .get("human.default_choice")
                .and_then(|v| v.as_str());
            if let Some(default_target) = default_choice {
                return Ok(make_choice_outcome(
                    default_target,
                    default_target,
                    default_target,
                ));
            }
            return Ok(Outcome::retry_classify("human gate timeout, no default"));
        }

        if answer.value == AnswerValue::Cancelled {
            return Err(Error::Cancelled);
        }

        // 5. Handle unanswered / interrupted interview sessions.
        if answer.value == AnswerValue::Interrupted {
            if services
                .cancel_requested
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                return Err(Error::Cancelled);
            }
            self.emit(
                &services.emitter,
                &Event::InterviewInterrupted {
                    question_id: question_id.clone(),
                    question: question_text,
                    stage: node.id.clone(),
                    reason: "interrupted".to_string(),
                    duration_ms: millis_u64(interview_start.elapsed()),
                },
                &stage_scope,
            );
            return Ok(unanswered_human_gate(
                "human interaction interrupted before an answer was provided",
            ));
        }
        if answer.value == AnswerValue::Skipped {
            self.emit(
                &services.emitter,
                &Event::InterviewCompleted {
                    question_id,
                    question: question_text,
                    answer: answer_text(&answer),
                    duration_ms: millis_u64(interview_start.elapsed()),
                },
                &stage_scope,
            );
            return Ok(unanswered_human_gate("human skipped interaction"));
        }

        // Emit interview completed for successful interactions
        self.emit(
            &services.emitter,
            &Event::InterviewCompleted {
                question_id,
                question: question_text,
                answer: answer_text(&answer),
                duration_ms: millis_u64(interview_start.elapsed()),
            },
            &stage_scope,
        );

        // 6. Try fixed-choice match
        if let Some(selected) = find_choice_match(&answer, &choices) {
            return Ok(make_choice_outcome(
                &selected.key,
                &selected.label,
                &selected.to,
            ));
        }

        // 7. Freeform fallback
        if let Some(freeform_to) = &freeform_target {
            let text = answer_text(&answer);
            let mut outcome = Outcome::success();
            outcome.suggested_next_ids = vec![freeform_to.clone()];
            outcome.context_updates.insert(
                keys::HUMAN_GATE_SELECTED.to_string(),
                serde_json::json!("freeform"),
            );
            outcome
                .context_updates
                .insert(keys::HUMAN_GATE_LABEL.to_string(), serde_json::json!(text));
            outcome
                .context_updates
                .insert(keys::HUMAN_GATE_TEXT.to_string(), serde_json::json!(text));
            return Ok(outcome);
        }

        // 8. Fallback to first choice
        if let Some(first) = choices.first() {
            return Ok(make_choice_outcome(&first.key, &first.label, &first.to));
        }

        Ok(Outcome::fail_deterministic("No matching choice"))
    }
}

fn make_choice_outcome(key: &str, label: &str, to: &str) -> Outcome {
    let mut outcome = Outcome::success();
    outcome.preferred_label = Some(label.to_string());
    outcome.suggested_next_ids = vec![to.to_string()];
    outcome.context_updates.insert(
        keys::HUMAN_GATE_SELECTED.to_string(),
        serde_json::json!(key),
    );
    outcome
        .context_updates
        .insert(keys::HUMAN_GATE_LABEL.to_string(), serde_json::json!(label));
    outcome
}

fn unanswered_human_gate(reason: impl Into<String>) -> Outcome {
    Outcome::fail_deterministic(reason)
}

fn find_choice_match<'a>(answer: &Answer, choices: &'a [Choice]) -> Option<&'a Choice> {
    match &answer.value {
        AnswerValue::Selected(key) => choices.iter().find(|c| c.key == *key),
        AnswerValue::Text(text) => {
            // Try matching by key or label
            choices
                .iter()
                .find(|c| c.key.eq_ignore_ascii_case(text) || c.label.eq_ignore_ascii_case(text))
        }
        _ => None,
    }
}

fn answer_text(answer: &Answer) -> String {
    if let Some(text) = &answer.text {
        return text.clone();
    }
    match &answer.value {
        AnswerValue::Text(t) => t.clone(),
        AnswerValue::Selected(s) => s.clone(),
        AnswerValue::MultiSelected(keys) => keys.join(", "),
        AnswerValue::Yes => "yes".to_string(),
        AnswerValue::No => "no".to_string(),
        AnswerValue::Cancelled => "cancelled".to_string(),
        AnswerValue::Interrupted => "interrupted".to_string(),
        AnswerValue::Skipped => "skipped".to_string(),
        AnswerValue::Timeout => "timeout".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fabro_graphviz::graph::{AttrValue, Edge};
    use fabro_interview::{AutoApproveInterviewer, CallbackInterviewer, RecordingInterviewer};

    use super::*;
    use crate::event::EventBody;

    fn make_services() -> EngineServices {
        EngineServices::test_default()
    }

    fn make_services_with_events(events: Arc<Mutex<Vec<fabro_types::RunEvent>>>) -> EngineServices {
        let mut services = EngineServices::test_default();
        let emitter = Arc::new(Emitter::default());
        emitter.on_event(move |event| {
            events
                .lock()
                .expect("event log lock poisoned")
                .push(event.clone());
        });
        services.emitter = emitter;
        services
    }

    fn build_graph_with_human_gate() -> Graph {
        let mut graph = Graph::new("test");
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "shape".to_string(),
            AttrValue::String("hexagon".to_string()),
        );
        gate.attrs.insert(
            "label".to_string(),
            AttrValue::String("Review Changes".to_string()),
        );
        graph.nodes.insert("gate".to_string(), gate);
        graph
            .nodes
            .insert("approve".to_string(), Node::new("approve"));
        graph
            .nodes
            .insert("reject".to_string(), Node::new("reject"));

        let mut e1 = Edge::new("gate", "approve");
        e1.attrs.insert(
            "label".to_string(),
            AttrValue::String("[A] Approve".to_string()),
        );
        let mut e2 = Edge::new("gate", "reject");
        e2.attrs.insert(
            "label".to_string(),
            AttrValue::String("[R] Reject".to_string()),
        );
        graph.edges.push(e1);
        graph.edges.push(e2);
        graph
    }

    #[test]
    fn parse_accelerator_key_bracket() {
        assert_eq!(parse_accelerator_key("[A] Approve"), "A");
        assert_eq!(parse_accelerator_key("[Y] Yes, deploy"), "Y");
    }

    #[test]
    fn parse_accelerator_key_paren() {
        assert_eq!(parse_accelerator_key("Y) Yes, deploy"), "Y");
    }

    #[test]
    fn parse_accelerator_key_dash() {
        assert_eq!(parse_accelerator_key("Y - Yes, deploy"), "Y");
    }

    #[test]
    fn parse_accelerator_key_first_char() {
        assert_eq!(parse_accelerator_key("Yes, deploy"), "Y");
    }

    #[test]
    fn parse_accelerator_key_empty() {
        assert_eq!(parse_accelerator_key(""), "");
    }

    #[tokio::test]
    async fn wait_human_auto_approve_selects_first() {
        let interviewer = Arc::new(AutoApproveInterviewer);
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        // Auto-approve picks first option key "A"
        assert_eq!(
            outcome.context_updates.get(keys::HUMAN_GATE_SELECTED),
            Some(&serde_json::json!("A"))
        );
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
    }

    #[tokio::test]
    async fn wait_human_no_edges_returns_fail() {
        let interviewer = Arc::new(AutoApproveInterviewer);
        let handler = HumanHandler::new(interviewer);
        let mut graph = Graph::new("test");
        let gate = Node::new("gate");
        graph.nodes.insert("gate".to_string(), gate);
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
    }

    #[tokio::test]
    async fn wait_human_interrupted_returns_fail_without_routing_hints() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::interrupted()));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert!(outcome.preferred_label.is_none());
        assert!(outcome.suggested_next_ids.is_empty());
        assert_eq!(
            outcome.failure_reason(),
            Some("human interaction interrupted before an answer was provided")
        );
    }

    #[tokio::test]
    async fn wait_human_cancelled_returns_cancelled_error() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::cancelled()));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let error = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Cancelled));
    }

    #[tokio::test]
    async fn wait_human_skipped_returns_fail_without_routing_hints() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::skipped()));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
        assert!(outcome.preferred_label.is_none());
        assert!(outcome.suggested_next_ids.is_empty());
        assert_eq!(outcome.failure_reason(), Some("human skipped interaction"));
    }

    #[tokio::test]
    async fn wait_human_interrupted_emits_interview_interrupted_event() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::interrupted()));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");
        let events = Arc::new(Mutex::new(Vec::new()));

        let _ = handler
            .execute(
                node,
                &context,
                &graph,
                run_dir,
                &make_services_with_events(Arc::clone(&events)),
            )
            .await
            .unwrap();

        assert!(
            events
                .lock()
                .expect("event log lock poisoned")
                .iter()
                .any(|event| matches!(
                    &event.body,
                    EventBody::InterviewInterrupted(props)
                        if props.reason == "interrupted"
                ))
        );
    }

    #[tokio::test]
    async fn wait_human_skipped_emits_interview_completed_event() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| Answer::skipped()));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");
        let events = Arc::new(Mutex::new(Vec::new()));

        let _ = handler
            .execute(
                node,
                &context,
                &graph,
                run_dir,
                &make_services_with_events(Arc::clone(&events)),
            )
            .await
            .unwrap();

        assert!(
            events
                .lock()
                .expect("event log lock poisoned")
                .iter()
                .any(|event| matches!(
                    &event.body,
                    EventBody::InterviewCompleted(props)
                        if props.answer == "skipped"
                ))
        );
    }

    #[tokio::test]
    async fn wait_human_with_freeform_edge() {
        let interviewer = Arc::new(fabro_interview::CallbackInterviewer::new(|_| {
            Answer::text("custom input")
        }));
        let handler = HumanHandler::new(interviewer);

        let mut graph = Graph::new("test");
        let mut gate = Node::new("gate");
        gate.attrs
            .insert("label".to_string(), AttrValue::String("Choose".to_string()));
        graph.nodes.insert("gate".to_string(), gate);
        graph
            .nodes
            .insert("freeform_target".to_string(), Node::new("freeform_target"));

        let mut edge = Edge::new("gate", "freeform_target");
        edge.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        graph.edges.push(edge);

        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(outcome.suggested_next_ids, vec!["freeform_target"]);
        assert_eq!(
            outcome.context_updates.get(keys::HUMAN_GATE_TEXT),
            Some(&serde_json::json!("custom input"))
        );
    }

    #[tokio::test]
    async fn freeform_only_gate_uses_freeform_question_type() {
        let inner = Box::new(fabro_interview::CallbackInterviewer::new(|_| {
            Answer::text("hello")
        }));
        let recorder = Arc::new(RecordingInterviewer::new(inner));
        let handler = HumanHandler::new(recorder.clone());

        let mut graph = Graph::new("test");
        let mut gate = Node::new("gate");
        gate.attrs.insert(
            "label".to_string(),
            AttrValue::String("Enter prompt".to_string()),
        );
        graph.nodes.insert("gate".to_string(), gate);
        graph
            .nodes
            .insert("target".to_string(), Node::new("target"));

        let mut edge = Edge::new("gate", "target");
        edge.attrs
            .insert("freeform".to_string(), AttrValue::Boolean(true));
        graph.edges.push(edge);

        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();

        let recordings = recorder.recordings();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].0.question_type, QuestionType::Freeform);
    }

    #[tokio::test]
    async fn simulate_selects_first_choice() {
        let interviewer = Arc::new(AutoApproveInterviewer);
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .simulate(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert!(outcome.notes.as_deref().unwrap().contains("[Simulated]"));
        assert_eq!(
            outcome.context_updates.get(keys::HUMAN_GATE_SELECTED),
            Some(&serde_json::json!("A"))
        );
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
    }
}
