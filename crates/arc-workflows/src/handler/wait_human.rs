use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use crate::context::Context;
use crate::error::ArcError;
use crate::event::{EventEmitter, PipelineEvent};
use crate::graph::{Graph, Node};
use crate::interviewer::{
    Answer, AnswerValue, Interviewer, Question, QuestionOption, QuestionType,
};
use crate::outcome::Outcome;

use super::{EngineServices, Handler};

/// Convert a Duration's milliseconds to u64, saturating on overflow.
fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

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
pub struct WaitHumanHandler {
    interviewer: Arc<dyn Interviewer>,
    emitter: Option<Arc<EventEmitter>>,
}

impl WaitHumanHandler {
    pub fn new(interviewer: Arc<dyn Interviewer>) -> Self {
        Self {
            interviewer,
            emitter: None,
        }
    }

    #[must_use]
    pub fn with_emitter(mut self, emitter: Arc<EventEmitter>) -> Self {
        self.emitter = Some(emitter);
        self
    }

    fn emit(&self, event: &PipelineEvent) {
        if let Some(emitter) = &self.emitter {
            emitter.emit(event);
        }
    }
}

#[async_trait]
impl Handler for WaitHumanHandler {
    async fn execute(
        &self,
        node: &Node,
        _context: &Context,
        graph: &Graph,
        _logs_root: &Path,
        _services: &EngineServices,
    ) -> Result<Outcome, ArcError> {
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
            return Ok(Outcome::fail("No outgoing edges for human gate"));
        }

        // 2. Build question
        let options: Vec<QuestionOption> = choices
            .iter()
            .map(|c| QuestionOption {
                key: c.key.clone(),
                label: c.label.clone(),
            })
            .collect();

        let mut question = Question::new(node.label(), QuestionType::MultipleChoice);
        question.options = options;
        question.allow_freeform = freeform_target.is_some();
        question.stage.clone_from(&node.id);

        // 3. Present to interviewer
        let question_text = node.label().to_string();
        self.emit(&PipelineEvent::InterviewStarted {
            question: question_text.clone(),
            stage: node.id.clone(),
            question_type: question.question_type.to_string(),
        });
        let interview_start = Instant::now();
        let answer = self.interviewer.ask(question).await;

        // 4. Handle timeout
        if answer.value == AnswerValue::Timeout {
            self.emit(&PipelineEvent::InterviewTimeout {
                question: question_text,
                stage: node.id.clone(),
                duration_ms: millis_u64(interview_start.elapsed()),
            });
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
            return Ok(Outcome::retry("human gate timeout, no default"));
        }

        // 5. Handle skipped
        if answer.value == AnswerValue::Skipped {
            return Ok(Outcome::fail("human skipped interaction"));
        }

        // Emit interview completed for successful interactions
        self.emit(&PipelineEvent::InterviewCompleted {
            question: question_text,
            answer: answer_text(&answer),
            duration_ms: millis_u64(interview_start.elapsed()),
        });

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
                "human.gate.selected".to_string(),
                serde_json::json!("freeform"),
            );
            outcome
                .context_updates
                .insert("human.gate.label".to_string(), serde_json::json!(text));
            outcome
                .context_updates
                .insert("human.gate.text".to_string(), serde_json::json!(text));
            return Ok(outcome);
        }

        // 8. Fallback to first choice
        if let Some(first) = choices.first() {
            return Ok(make_choice_outcome(&first.key, &first.label, &first.to));
        }

        Ok(Outcome::fail("No matching choice"))
    }
}

fn make_choice_outcome(key: &str, label: &str, to: &str) -> Outcome {
    let mut outcome = Outcome::success();
    outcome.preferred_label = Some(label.to_string());
    outcome.suggested_next_ids = vec![to.to_string()];
    outcome
        .context_updates
        .insert("human.gate.selected".to_string(), serde_json::json!(key));
    outcome
        .context_updates
        .insert("human.gate.label".to_string(), serde_json::json!(label));
    outcome
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
        AnswerValue::Yes => "yes".to_string(),
        AnswerValue::No => "no".to_string(),
        AnswerValue::Skipped => "skipped".to_string(),
        AnswerValue::Timeout => "timeout".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventEmitter;
    use crate::graph::{AttrValue, Edge};
    use crate::handler::start::StartHandler;
    use crate::handler::HandlerRegistry;
    use crate::interviewer::auto_approve::AutoApproveInterviewer;

    fn make_services() -> EngineServices {
        EngineServices {
            registry: std::sync::Arc::new(HandlerRegistry::new(Box::new(StartHandler))),
            emitter: std::sync::Arc::new(EventEmitter::new()),
            execution_env: std::sync::Arc::new(arc_agent::LocalExecutionEnvironment::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
        }
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
        let handler = WaitHumanHandler::new(interviewer);
        let graph = build_graph_with_human_gate();
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        // Auto-approve picks first option key "A"
        assert_eq!(
            outcome.context_updates.get("human.gate.selected"),
            Some(&serde_json::json!("A"))
        );
        assert_eq!(outcome.suggested_next_ids, vec!["approve"]);
    }

    #[tokio::test]
    async fn wait_human_no_edges_returns_fail() {
        let interviewer = Arc::new(AutoApproveInterviewer);
        let handler = WaitHumanHandler::new(interviewer);
        let mut graph = Graph::new("test");
        let gate = Node::new("gate");
        graph.nodes.insert("gate".to_string(), gate);
        let node = graph.nodes.get("gate").unwrap();
        let context = Context::new();
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Fail);
    }

    #[tokio::test]
    async fn wait_human_with_freeform_edge() {
        let interviewer = Arc::new(crate::interviewer::callback::CallbackInterviewer::new(
            |_| Answer::text("custom input"),
        ));
        let handler = WaitHumanHandler::new(interviewer);

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
        let logs_root = Path::new("/tmp/test");

        let outcome = handler
            .execute(node, &context, &graph, logs_root, &make_services())
            .await
            .unwrap();
        assert_eq!(outcome.status, crate::outcome::StageStatus::Success);
        assert_eq!(outcome.suggested_next_ids, vec!["freeform_target"]);
        assert_eq!(
            outcome.context_updates.get("human.gate.text"),
            Some(&serde_json::json!("custom input"))
        );
    }
}
