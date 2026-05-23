use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use fabro_graphviz::graph::{Graph, Node};
use fabro_interview::{Answer, AnswerValue, Interviewer, Question, ask_with_timeout};
use fabro_types::{InterviewOption, Principal, QuestionType, SystemActorKind};
use ulid::Ulid;

use super::{EngineServices, Handler, NodeTimeoutPolicy};
use crate::context::{Context, keys};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::millis_u64;
use crate::outcome::{Outcome, OutcomeExt};

/// A choice derived from an outgoing edge.
struct Choice {
    key:   String,
    label: String,
    to:    String,
}

struct ChoiceMatch<'a> {
    route:          &'a Choice,
    selected_key:   String,
    selected_label: String,
}

struct HumanGateQuestion {
    choices:         Vec<Choice>,
    freeform_target: Option<String>,
    question:        Question,
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

fn build_human_gate_question(
    node: &Node,
    context: &Context,
    graph: &Graph,
) -> Result<HumanGateQuestion, String> {
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
        return Err("No outgoing edges for human gate".to_string());
    }

    let question_type = question_type_for_node(node, choices.is_empty())?;
    let mut question = Question::new(node.label(), question_type);
    question.id = Ulid::new().to_string();
    question.options = choices
        .iter()
        .map(|choice| InterviewOption {
            key:         choice.key.clone(),
            label:       choice.label.clone(),
            description: None,
            preview:     None,
        })
        .collect();
    question.allow_freeform = freeform_target.is_some();
    question.stage.clone_from(&node.id);
    question.timeout_seconds = node.timeout().map(|duration| duration.as_secs_f64());

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

    Ok(HumanGateQuestion {
        choices,
        freeform_target,
        question,
    })
}

/// Blocks until a human selects an option derived from outgoing edges.
pub struct HumanHandler {
    interviewer: Arc<dyn Interviewer>,
    emitter:     Option<Arc<Emitter>>,
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
        let HumanGateQuestion {
            choices,
            freeform_target,
            question,
        } = match build_human_gate_question(node, context, graph) {
            Ok(question) => question,
            Err(reason) => return Ok(Outcome::fail_deterministic(reason)),
        };

        // Present to interviewer
        let question_text = question.text.clone();
        let question_id = question.id.clone();
        let stage_scope = StageScope::for_handler(context, &node.id);
        self.emit(
            &services.run.emitter,
            &Event::InterviewStarted {
                question_id:     question_id.clone(),
                question:        question_text.clone(),
                stage:           node.id.clone(),
                question_type:   question.question_type.to_string(),
                options:         question
                    .options
                    .iter()
                    .map(|option| InterviewOption {
                        key:         option.key.clone(),
                        label:       option.label.clone(),
                        description: option.description.clone(),
                        preview:     option.preview.clone(),
                    })
                    .collect(),
                allow_freeform:  question.allow_freeform,
                timeout_seconds: question.timeout_seconds,
                context_display: question.context_display.clone(),
            },
            &stage_scope,
        );
        let interview_guard = services
            .run
            .interview_blocker
            .block(Arc::clone(&services.run.emitter));
        let interview_start = Instant::now();
        let answer_submission = ask_with_timeout(self.interviewer.as_ref(), question).await;
        let answer_actor = answer_submission.actor.clone();
        let answer = answer_submission.answer;

        // Handle timeout
        if answer.value == AnswerValue::Timeout {
            self.emit(
                &services.run.emitter,
                &Event::InterviewTimeout {
                    actor:       Some(Principal::System {
                        system_kind: SystemActorKind::Timeout,
                    }),
                    question_id: question_id.clone(),
                    question:    question_text.clone(),
                    stage:       node.id.clone(),
                    duration_ms: millis_u64(interview_start.elapsed()),
                },
                &stage_scope,
            );
            interview_guard.resolve();
            let default_choice = node
                .attrs
                .get("human.default_choice")
                .and_then(|v| v.as_str());
            if let Some(default_target) = default_choice {
                let mut outcome =
                    make_choice_outcome(default_target, default_target, default_target);
                add_answer_context(
                    &mut outcome,
                    &node.id,
                    &question_text,
                    "timeout",
                    Some(default_target),
                );
                return Ok(outcome);
            }
            return Ok(Outcome::retry_classify("human gate timeout, no default"));
        }

        if answer.value == AnswerValue::Cancelled {
            return Err(Error::Cancelled);
        }

        // Handle unanswered / interrupted interview sessions.
        if answer.value == AnswerValue::Interrupted {
            if services.run.cancel_token().is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.emit(
                &services.run.emitter,
                &Event::InterviewInterrupted {
                    actor:       Some(Principal::System {
                        system_kind: SystemActorKind::Engine,
                    }),
                    question_id: question_id.clone(),
                    question:    question_text.clone(),
                    stage:       node.id.clone(),
                    reason:      "interrupted".to_string(),
                    duration_ms: millis_u64(interview_start.elapsed()),
                },
                &stage_scope,
            );
            interview_guard.resolve();
            return Ok(unanswered_human_gate(
                "human interaction interrupted before an answer was provided",
            ));
        }
        if answer.value == AnswerValue::Skipped {
            self.emit(
                &services.run.emitter,
                &Event::InterviewCompleted {
                    actor: Some(answer_actor),
                    question_id,
                    question: question_text.clone(),
                    answer: answer_text(&answer),
                    duration_ms: millis_u64(interview_start.elapsed()),
                },
                &stage_scope,
            );
            interview_guard.resolve();
            return Ok(unanswered_human_gate("human skipped interaction"));
        }

        // Emit interview completed for successful interactions
        self.emit(
            &services.run.emitter,
            &Event::InterviewCompleted {
                actor: Some(answer_actor),
                question_id,
                question: question_text.clone(),
                answer: answer_text(&answer),
                duration_ms: millis_u64(interview_start.elapsed()),
            },
            &stage_scope,
        );
        interview_guard.resolve();

        // Try fixed-choice match
        if let Some(selected) = find_choice_match(&answer, &choices) {
            let mut outcome = make_choice_outcome(
                &selected.selected_key,
                &selected.selected_label,
                &selected.route.to,
            );
            add_answer_context(
                &mut outcome,
                &node.id,
                &question_text,
                &answer_text(&answer),
                Some(&selected.selected_label),
            );
            return Ok(outcome);
        }

        // Freeform fallback
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
            add_answer_context(
                &mut outcome,
                &node.id,
                &question_text,
                &answer_text(&answer),
                None,
            );
            return Ok(outcome);
        }

        // Fallback to first choice
        if let Some(first) = choices.first() {
            let mut outcome = make_choice_outcome(&first.key, &first.label, &first.to);
            add_answer_context(
                &mut outcome,
                &node.id,
                &question_text,
                &answer_text(&answer),
                Some(&first.label),
            );
            return Ok(outcome);
        }

        Ok(Outcome::fail_deterministic("No matching choice"))
    }

    fn node_timeout_policy(&self, _node: &Node) -> NodeTimeoutPolicy {
        NodeTimeoutPolicy::HandlerManaged
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

fn question_type_for_node(node: &Node, default_freeform: bool) -> Result<QuestionType, String> {
    if let Some(value) = node
        .attrs
        .get("question_type")
        .and_then(|value| value.as_str())
    {
        return QuestionType::from_str(value)
            .map_err(|_| format!("invalid human question_type: {value}"));
    }

    if default_freeform {
        Ok(QuestionType::Freeform)
    } else {
        Ok(QuestionType::MultipleChoice)
    }
}

fn find_choice_match<'a>(answer: &Answer, choices: &'a [Choice]) -> Option<ChoiceMatch<'a>> {
    match &answer.value {
        AnswerValue::Selected(key) => {
            choices
                .iter()
                .find(|choice| choice.key == *key)
                .map(|choice| ChoiceMatch {
                    route:          choice,
                    selected_key:   choice.key.clone(),
                    selected_label: choice.label.clone(),
                })
        }
        AnswerValue::MultiSelected(keys) => {
            let selected: Vec<&Choice> = keys
                .iter()
                .filter_map(|key| choices.iter().find(|choice| choice.key == *key))
                .collect();
            selected.first().map(|first| ChoiceMatch {
                route:          first,
                selected_key:   selected
                    .iter()
                    .map(|choice| choice.key.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                selected_label: selected
                    .iter()
                    .map(|choice| choice.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        }
        AnswerValue::Yes => find_yes_no_choice(choices, true).map(|choice| ChoiceMatch {
            route:          choice,
            selected_key:   choice.key.clone(),
            selected_label: choice.label.clone(),
        }),
        AnswerValue::No => find_yes_no_choice(choices, false).map(|choice| ChoiceMatch {
            route:          choice,
            selected_key:   choice.key.clone(),
            selected_label: choice.label.clone(),
        }),
        AnswerValue::Text(text) => {
            // Try matching by key or label
            choices
                .iter()
                .find(|c| c.key.eq_ignore_ascii_case(text) || c.label.eq_ignore_ascii_case(text))
                .map(|choice| ChoiceMatch {
                    route:          choice,
                    selected_key:   choice.key.clone(),
                    selected_label: choice.label.clone(),
                })
        }
        _ => None,
    }
}

fn find_yes_no_choice(choices: &[Choice], yes: bool) -> Option<&Choice> {
    let expected_keys = if yes {
        &["Y", "YES"][..]
    } else {
        &["N", "NO"][..]
    };
    let expected_word = if yes { "yes" } else { "no" };

    choices.iter().find(|choice| {
        expected_keys
            .iter()
            .any(|expected| choice.key.eq_ignore_ascii_case(expected))
            || choice.label.eq_ignore_ascii_case(expected_word)
    })
}

fn add_answer_context(
    outcome: &mut Outcome,
    node_id: &str,
    question: &str,
    answer: &str,
    selected_label: Option<&str>,
) {
    outcome.context_updates.insert(
        format!("human.gate.{node_id}.question"),
        serde_json::json!(question),
    );
    outcome.context_updates.insert(
        format!("human.gate.{node_id}.answer"),
        serde_json::json!(answer),
    );
    if let Some(label) = selected_label {
        outcome.context_updates.insert(
            format!("human.gate.{node_id}.label"),
            serde_json::json!(label),
        );
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
    use std::time::Duration;

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
        services.run = services.run.with_emitter(emitter);
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

    fn build_graph_with_typed_gate(question_type: &str) -> Graph {
        let mut graph = build_graph_with_human_gate();
        graph.nodes.get_mut("gate").unwrap().attrs.insert(
            "question_type".to_string(),
            AttrValue::String(question_type.to_string()),
        );
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
        let interviewer = Arc::new(AutoApproveInterviewer::engine());
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);
        // Auto-approve picks first option key "A"
        assert_eq!(
            outcome.context_updates.get(keys::HUMAN_GATE_SELECTED),
            Some(&serde_json::json!("A"))
        );
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
    }

    #[tokio::test]
    async fn wait_human_no_edges_returns_fail() {
        let interviewer = Arc::new(AutoApproveInterviewer::engine());
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
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: false,
        });
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

        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: false,
        });
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

        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: false,
        });
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
    async fn wait_human_emits_blocked_then_unblocked_around_interview() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| {
            Answer::selected("A", InterviewOption {
                key:         "A".to_string(),
                label:       "Approve".to_string(),
                description: None,
                preview:     None,
            })
        }));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");
        let events = Arc::new(Mutex::new(Vec::new()));

        handler
            .execute(
                node,
                &context,
                &graph,
                run_dir,
                &make_services_with_events(Arc::clone(&events)),
            )
            .await
            .unwrap();

        let event_names = events
            .lock()
            .expect("event log lock poisoned")
            .iter()
            .map(|event| event.event_name().to_string())
            .collect::<Vec<_>>();

        assert_eq!(event_names, vec![
            "interview.started",
            "run.blocked",
            "interview.completed",
            "run.unblocked",
        ]);
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
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);
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
    async fn explicit_yes_no_gate_uses_yes_no_question_type() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = Arc::new(RecordingInterviewer::new(inner));
        let handler = HumanHandler::new(recorder.clone());
        let graph = build_graph_with_typed_gate("yes_no");
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();

        let recordings = recorder.recordings();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].0.question_type, QuestionType::YesNo);
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
        assert_eq!(
            outcome.context_updates.get("human.gate.gate.answer"),
            Some(&serde_json::json!("yes"))
        );
    }

    #[tokio::test]
    async fn wait_human_copies_node_timeout_to_question_and_started_event() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = Arc::new(RecordingInterviewer::new(inner));
        let handler = HumanHandler::new(recorder.clone());
        let mut graph = build_graph_with_human_gate();
        let timeout = Duration::from_millis(125);
        let timeout_seconds = timeout.as_secs_f64();
        graph
            .nodes
            .get_mut("gate")
            .unwrap()
            .attrs
            .insert("timeout".to_string(), AttrValue::Duration(timeout));
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");
        let events = Arc::new(Mutex::new(Vec::new()));

        handler
            .execute(
                node,
                &context,
                &graph,
                run_dir,
                &make_services_with_events(Arc::clone(&events)),
            )
            .await
            .unwrap();

        let recordings = recorder.recordings();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].0.timeout_seconds, Some(timeout_seconds));

        let started_timeout = events
            .lock()
            .expect("event log lock poisoned")
            .iter()
            .find_map(|event| match &event.body {
                EventBody::InterviewStarted(props) => props.timeout_seconds,
                _ => None,
            });
        assert_eq!(started_timeout, Some(timeout_seconds));
    }

    #[tokio::test]
    async fn explicit_multi_select_gate_records_all_selected_keys() {
        let interviewer = Arc::new(CallbackInterviewer::new(|_| {
            Answer::multi_selected(vec!["A".to_string(), "R".to_string()])
        }));
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_typed_gate("multi_select");
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();

        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
        assert_eq!(
            outcome.context_updates.get(keys::HUMAN_GATE_SELECTED),
            Some(&serde_json::json!("A,R"))
        );
        assert_eq!(
            outcome.context_updates.get("human.gate.gate.answer"),
            Some(&serde_json::json!("A, R"))
        );
    }

    #[tokio::test]
    async fn simulate_selects_first_choice() {
        let interviewer = Arc::new(AutoApproveInterviewer::engine());
        let handler = HumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let run_dir = Path::new("/tmp/test");

        let outcome = handler
            .simulate(node, &context, &graph, run_dir, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageOutcome::Succeeded);
        assert!(outcome.notes.as_deref().unwrap().contains("[Simulated]"));
        assert_eq!(
            outcome.context_updates.get(keys::HUMAN_GATE_SELECTED),
            Some(&serde_json::json!("A"))
        );
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
    }

    #[test]
    fn blocked_state_tracker_emits_once_across_parallel_interview_races() {
        let blocker = Arc::new(crate::interview_runtime::RunInterviewBlocker::new());
        let emitter = Arc::new(Emitter::new(fabro_types::fixtures::RUN_1));
        let event_names = Arc::new(Mutex::new(Vec::new()));
        let guards = Arc::new(Mutex::new(Vec::new()));

        emitter.on_event({
            let event_names = Arc::clone(&event_names);
            move |event| {
                let name = match &event.body {
                    EventBody::RunBlocked(_) => Some("run.blocked"),
                    EventBody::RunUnblocked(_) => Some("run.unblocked"),
                    _ => None,
                };
                if let Some(name) = name {
                    event_names.lock().unwrap().push(name.to_string());
                }
            }
        });

        std::thread::scope(|scope| {
            for _ in 0..8 {
                let blocker = Arc::clone(&blocker);
                let emitter = Arc::clone(&emitter);
                let guards = Arc::clone(&guards);
                scope.spawn(move || {
                    guards.lock().unwrap().push(blocker.block(emitter));
                });
            }
        });

        std::thread::scope(|scope| {
            for _ in 0..8 {
                let guards = Arc::clone(&guards);
                scope.spawn(move || {
                    let guard = guards.lock().unwrap().pop().unwrap();
                    guard.resolve();
                });
            }
        });

        assert_eq!(event_names.lock().unwrap().as_slice(), [
            "run.blocked",
            "run.unblocked"
        ],);
    }
}
