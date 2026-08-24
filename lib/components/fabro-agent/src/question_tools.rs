//! Model-native tools that let a root workflow agent ask the human for input.

use std::collections::BTreeMap;
use std::future::Future;
use std::ops::RangeInclusive;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_llm::types::ToolDefinition;
use fabro_model::AgentProfileKind;
use fabro_types::{InterviewOption, QuestionType};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::tool_registry::{RegisteredTool, ToolContext, ToolRegistry, ToolSource};

tokio::task_local! {
    static CURRENT_AGENT_TOOL_RUNTIME: AgentToolRuntime;
}

pub const OPENAI_REQUEST_USER_INPUT_TOOL: &str = "request_user_input";
pub const ANTHROPIC_ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";

pub const OPTION_DESCRIPTION_MAX_CHARS: usize = 2_000;
pub const OPTION_PREVIEW_MAX_CHARS: usize = 4_000;

const ROOT_SESSION_REQUIRED_ERROR: &str =
    "human-question tools are available only during a root workflow agent session";

#[derive(Clone, Default)]
pub struct AgentToolRuntime {
    question_runtime: Option<Arc<dyn AgentQuestionRuntime>>,
}

impl AgentToolRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_question_runtime(runtime: Arc<dyn AgentQuestionRuntime>) -> Self {
        Self {
            question_runtime: Some(runtime),
        }
    }

    #[must_use]
    pub fn question_runtime(&self) -> Option<Arc<dyn AgentQuestionRuntime>> {
        self.question_runtime.clone()
    }
}

pub async fn scope_agent_tool_runtime<F>(runtime: AgentToolRuntime, future: F) -> F::Output
where
    F: Future,
{
    CURRENT_AGENT_TOOL_RUNTIME.scope(runtime, future).await
}

fn current_agent_tool_runtime() -> AgentToolRuntime {
    CURRENT_AGENT_TOOL_RUNTIME
        .try_with(Clone::clone)
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentQuestion {
    pub original_id:       Option<String>,
    pub original_question: String,
    pub header:            Option<String>,
    pub text:              String,
    pub question_type:     QuestionType,
    pub options:           Vec<InterviewOption>,
    pub allow_freeform:    bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentQuestionAnswerStatus {
    Answered,
    Cancelled,
    Interrupted,
    Skipped,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentQuestionAnswer {
    pub original_id:       Option<String>,
    pub original_question: String,
    pub answers:           Vec<String>,
    pub status:            AgentQuestionAnswerStatus,
}

#[async_trait]
pub trait AgentQuestionRuntime: Send + Sync {
    async fn ask_questions(
        &self,
        tool_call_id: &str,
        questions: Vec<AgentQuestion>,
        cancel_token: CancellationToken,
    ) -> Result<Vec<AgentQuestionAnswer>, String>;
}

#[derive(Debug, Deserialize)]
struct OpenAiQuestionToolArgs {
    questions: Vec<OpenAiQuestion>,
}

#[derive(Debug, Deserialize)]
struct OpenAiQuestion {
    id:       String,
    header:   String,
    question: String,
    #[serde(default)]
    options:  Vec<OpenAiOption>,
}

#[derive(Debug, Deserialize)]
struct OpenAiOption {
    label:       String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicQuestionToolArgs {
    questions: Vec<AnthropicQuestion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnthropicQuestion {
    question:     String,
    #[serde(default)]
    header:       Option<String>,
    #[serde(default)]
    options:      Vec<AnthropicOption>,
    #[serde(default)]
    multi_select: bool,
}

#[derive(Debug, Deserialize)]
struct AnthropicOption {
    label:       String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    preview:     Option<String>,
}

/// Contract rules the JSON Schema cannot express, and which differ between
/// the two harnesses sharing one normalizer.
struct QuestionLimits {
    questions: RangeInclusive<usize>,
    questions_error: &'static str,
    /// `None` leaves the option count unbounded.
    options: Option<RangeInclusive<usize>>,
    options_error: &'static str,
    max_header_chars: Option<usize>,
    /// Claude 5's schema marks `header` and every option `description`
    /// required, so both are validated rather than passed through as given.
    require_header_and_descriptions: bool,
    /// Claude 5 renders multi-select without a preview pane.
    allow_preview_with_multi_select: bool,
}

const ANTHROPIC_QUESTION_LIMITS: QuestionLimits = QuestionLimits {
    questions: 1..=usize::MAX,
    questions_error: "questions must contain at least one question",
    options: None,
    options_error: "",
    max_header_chars: None,
    require_header_and_descriptions: false,
    allow_preview_with_multi_select: true,
};

const CLAUDE5_QUESTION_LIMITS: QuestionLimits = QuestionLimits {
    questions: 1..=4,
    questions_error: "questions must contain between one and four questions",
    options: Some(2..=4),
    options_error: "each question must contain between two and four options",
    max_header_chars: Some(12),
    require_header_and_descriptions: true,
    allow_preview_with_multi_select: false,
};

#[must_use]
pub fn is_question_tool(name: &str) -> bool {
    matches!(
        name,
        OPENAI_REQUEST_USER_INPUT_TOOL | ANTHROPIC_ASK_USER_QUESTION_TOOL
    )
}

pub fn register_question_tools(profile_kind: AgentProfileKind, registry: &mut ToolRegistry) {
    match profile_kind {
        // Codex names this tool `request_user_input` for GPT-5.6 too.
        AgentProfileKind::OpenAi | AgentProfileKind::Gpt56 => {
            registry.register(make_openai_question_tool());
        }
        // Kimi Code names this tool `AskUserQuestion` with the same
        // question/option shape, so the Anthropic-style tool is a match.
        AgentProfileKind::Anthropic | AgentProfileKind::Kimi => {
            registry.register(make_anthropic_question_tool());
        }
        AgentProfileKind::Claude5 => {
            registry.register(make_claude5_question_tool());
        }
        AgentProfileKind::Gemini => {}
    }
}

fn make_openai_question_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        OPENAI_REQUEST_USER_INPUT_TOOL.to_string(),
            description: "Ask the human one or more questions and wait for their answers before continuing this stage.".to_string(),
            parameters:  json!({
                "type": "object",
                "required": ["questions"],
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "required": ["id", "header", "question", "options"],
                            "properties": {
                                "id": { "type": "string" },
                                "header": { "type": "string" },
                                "question": { "type": "string" },
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "required": ["label"],
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let parsed: OpenAiQuestionToolArgs = parse_tool_args(args)?;
                let questions = normalize_openai_questions(parsed)?;
                let answers = execute_question_tool(ctx, questions).await?;
                format_openai_answers(&answers)
            })
        }),
        source:     ToolSource::Native,
    }
}

fn make_anthropic_question_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        ANTHROPIC_ASK_USER_QUESTION_TOOL.to_string(),
            description: "Ask the human one or more questions and wait for their answers before continuing this stage.".to_string(),
            parameters:  json!({
                "type": "object",
                "required": ["questions"],
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "required": ["question", "options", "multiSelect"],
                            "properties": {
                                "question": { "type": "string" },
                                "header": { "type": "string" },
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "required": ["label"],
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" },
                                            "preview": { "type": "string" }
                                        }
                                    }
                                },
                                "multiSelect": { "type": "boolean" }
                            }
                        }
                    }
                }
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let parsed: AnthropicQuestionToolArgs = parse_tool_args(args)?;
                let questions =
                    normalize_anthropic_questions(parsed, &ANTHROPIC_QUESTION_LIMITS)?;
                let answers = execute_question_tool(ctx, questions).await?;
                format_anthropic_answers(&answers)
            })
        }),
        source:     ToolSource::Native,
    }
}

fn make_claude5_question_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        ANTHROPIC_ASK_USER_QUESTION_TOOL.to_string(),
            description: "Ask the human up to four questions when a decision is genuinely theirs to make. The UI automatically provides an Other option for custom text.".to_string(),
            parameters:  json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "description": "Questions to ask the user (1-4 questions)",
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {
                                    "description": "The complete, clear, and specific question to ask.",
                                    "type": "string"
                                },
                                "header": {
                                    "description": "Very short label displayed as a chip/tag (max 12 chars).",
                                    "type": "string"
                                },
                                "options": {
                                    "description": "Two to four choices. Do not include Other; the UI adds it automatically.",
                                    "type": "array",
                                    "minItems": 2,
                                    "maxItems": 4,
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {
                                                "description": "Concise display text for the option.",
                                                "type": "string"
                                            },
                                            "description": {
                                                "description": "What the option means and its relevant trade-offs.",
                                                "type": "string"
                                            },
                                            "preview": {
                                                "description": "Optional Markdown preview for single-select visual comparisons.",
                                                "type": "string"
                                            }
                                        },
                                        "required": ["label", "description"],
                                        "additionalProperties": false
                                    }
                                },
                                "multiSelect": {
                                    "description": "Whether the user may select multiple options.",
                                    "default": false,
                                    "type": "boolean"
                                }
                            },
                            "required": ["question", "header", "options", "multiSelect"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["questions"],
                "additionalProperties": false
            }),
        },
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let parsed: AnthropicQuestionToolArgs = parse_tool_args(args)?;
                let questions =
                    normalize_anthropic_questions(parsed, &CLAUDE5_QUESTION_LIMITS)?;
                let answers = execute_question_tool(ctx, questions).await?;
                format_anthropic_answers(&answers)
            })
        }),
        source:     ToolSource::Native,
    }
}

fn parse_tool_args<T: for<'de> Deserialize<'de>>(args: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|err| format!("invalid question tool arguments: {err}"))
}

async fn execute_question_tool(
    ctx: ToolContext,
    questions: Vec<AgentQuestion>,
) -> Result<Vec<AgentQuestionAnswer>, String> {
    let session_id = ctx
        .session_id
        .as_deref()
        .ok_or_else(|| ROOT_SESSION_REQUIRED_ERROR.to_string())?;
    let root_session_id = ctx
        .root_session_id
        .as_deref()
        .ok_or_else(|| ROOT_SESSION_REQUIRED_ERROR.to_string())?;
    if session_id != root_session_id {
        return Err(
            "human-question tools are only available to the root agent; subagents must report back to their parent".to_string(),
        );
    }
    let tool_call_id = ctx
        .tool_call_id
        .as_deref()
        .ok_or_else(|| "human-question tool call is missing a provider tool_call_id".to_string())?;
    let runtime = current_agent_tool_runtime().question_runtime().ok_or_else(|| {
        "human-question tools are available only inside a workflow run with an active interviewer".to_string()
    })?;
    runtime
        .ask_questions(tool_call_id, questions, ctx.cancel.clone())
        .await
}

fn normalize_openai_questions(args: OpenAiQuestionToolArgs) -> Result<Vec<AgentQuestion>, String> {
    if args.questions.is_empty() {
        return Err("questions must contain at least one question".to_string());
    }
    args.questions
        .into_iter()
        .map(|question| {
            let original_question = question.question.trim().to_string();
            Ok(AgentQuestion {
                original_id: Some(non_empty(&question.id, "question id")?),
                text: display_text(Some(question.header.as_str()), &question.question),
                header: Some(question.header),
                original_question,
                question_type: QuestionType::MultipleChoice,
                options: options_from_openai(question.options),
                allow_freeform: true,
            })
        })
        .collect()
}

fn normalize_anthropic_questions(
    args: AnthropicQuestionToolArgs,
    limits: &QuestionLimits,
) -> Result<Vec<AgentQuestion>, String> {
    if !limits.questions.contains(&args.questions.len()) {
        return Err(limits.questions_error.to_string());
    }

    args.questions
        .into_iter()
        .map(|question| {
            let original_question = non_empty(&question.question, "question")?;
            let header = if limits.require_header_and_descriptions {
                let header = non_empty(
                    question.header.as_deref().unwrap_or_default(),
                    "question header",
                )?;
                if limits
                    .max_header_chars
                    .is_some_and(|max| header.chars().count() > max)
                {
                    return Err(format!(
                        "question header must contain at most {} characters",
                        limits.max_header_chars.unwrap_or_default()
                    ));
                }
                Some(header)
            } else {
                question.header
            };

            if let Some(bounds) = &limits.options {
                if !bounds.contains(&question.options.len()) {
                    return Err(limits.options_error.to_string());
                }
            }
            if !limits.allow_preview_with_multi_select
                && question.multi_select
                && question
                    .options
                    .iter()
                    .any(|option| option.preview.is_some())
            {
                return Err(
                    "option previews are not supported for multi-select questions".to_string(),
                );
            }

            // The lenient contract renders the question and header exactly as
            // supplied; the strict one has already trimmed them.
            let text = if limits.require_header_and_descriptions {
                display_text(header.as_deref(), &original_question)
            } else {
                display_text(header.as_deref(), &question.question)
            };

            Ok(AgentQuestion {
                original_id: None,
                text,
                header,
                original_question,
                question_type: if question.multi_select {
                    QuestionType::MultiSelect
                } else {
                    QuestionType::MultipleChoice
                },
                options: options_from_anthropic(question.options, limits)?,
                allow_freeform: true,
            })
        })
        .collect()
}

fn options_from_openai(options: Vec<OpenAiOption>) -> Vec<InterviewOption> {
    options
        .into_iter()
        .enumerate()
        .map(|(idx, option)| InterviewOption {
            key:         option_key(idx),
            label:       option.label,
            description: option
                .description
                .map(|value| bounded_display_field(&value, OPTION_DESCRIPTION_MAX_CHARS)),
            preview:     None,
        })
        .collect()
}

fn options_from_anthropic(
    options: Vec<AnthropicOption>,
    limits: &QuestionLimits,
) -> Result<Vec<InterviewOption>, String> {
    options
        .into_iter()
        .enumerate()
        .map(|(idx, option)| {
            let (label, description) = if limits.require_header_and_descriptions {
                (
                    non_empty(&option.label, "option label")?,
                    Some(non_empty(
                        option.description.as_deref().unwrap_or_default(),
                        "option description",
                    )?),
                )
            } else {
                (option.label, option.description)
            };
            Ok(InterviewOption {
                key: option_key(idx),
                label,
                description: description
                    .map(|value| bounded_display_field(&value, OPTION_DESCRIPTION_MAX_CHARS)),
                preview: option
                    .preview
                    .map(|value| bounded_display_field(&value, OPTION_PREVIEW_MAX_CHARS)),
            })
        })
        .collect()
}

fn option_key(idx: usize) -> String {
    format!("option_{}", idx + 1)
}

fn non_empty(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn display_text(header: Option<&str>, question: &str) -> String {
    let header = header.map(str::trim).filter(|value| !value.is_empty());
    let question = question.trim();
    match (header, question.is_empty()) {
        (Some(header), false) => format!("{header}\n\n{question}"),
        (Some(header), true) => header.to_string(),
        (None, false) => question.to_string(),
        (None, true) => String::new(),
    }
}

fn bounded_display_field(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => value[..byte_idx].to_string(),
        None => value.to_string(),
    }
}

fn ensure_all_answered(answers: &[AgentQuestionAnswer]) -> Result<(), String> {
    if let Some(answer) = answers
        .iter()
        .find(|answer| answer.status != AgentQuestionAnswerStatus::Answered)
    {
        return Err(format!(
            "human-question request ended before the user answered `{}`: {}",
            answer.original_question,
            answer_status_label(answer.status)
        ));
    }
    Ok(())
}

fn answer_status_label(status: AgentQuestionAnswerStatus) -> &'static str {
    match status {
        AgentQuestionAnswerStatus::Answered => "answered",
        AgentQuestionAnswerStatus::Cancelled => "cancelled",
        AgentQuestionAnswerStatus::Interrupted => "interrupted",
        AgentQuestionAnswerStatus::Skipped => "skipped",
        AgentQuestionAnswerStatus::Timeout => "timed out",
    }
}

fn format_openai_answers(answers: &[AgentQuestionAnswer]) -> Result<String, String> {
    ensure_all_answered(answers)?;
    let mut answer_map = BTreeMap::new();
    for answer in answers {
        let Some(original_id) = answer.original_id.as_ref() else {
            return Err(
                "OpenAI question answer is missing the original model question id".to_string(),
            );
        };
        answer_map.insert(original_id.clone(), json!({ "answers": answer.answers }));
    }
    serde_json::to_string(&json!({ "answers": answer_map }))
        .map_err(|err| format!("failed to serialize answers: {err}"))
}

fn format_anthropic_answers(answers: &[AgentQuestionAnswer]) -> Result<String, String> {
    ensure_all_answered(answers)?;
    let pairs = answers
        .iter()
        .map(|answer| {
            let question = json!(answer.original_question);
            let answer_text = json!(answer.answers.join(", "));
            format!("{question}={answer_text}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "User has answered your questions: {pairs}. You can now continue with the task."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_tool::ToolVocabulary;
    use crate::test_support::MockSandbox;

    fn answered(
        original_id: Option<&str>,
        question: &str,
        answers: &[&str],
    ) -> AgentQuestionAnswer {
        AgentQuestionAnswer {
            original_id:       original_id.map(str::to_string),
            original_question: question.to_string(),
            answers:           answers.iter().map(|value| (*value).to_string()).collect(),
            status:            AgentQuestionAnswerStatus::Answered,
        }
    }

    #[test]
    fn openai_request_with_descriptions_normalizes_to_multiple_choice() {
        let args: OpenAiQuestionToolArgs = serde_json::from_value(json!({
            "questions": [{
                "id": "q1",
                "header": "Decision",
                "question": "Which path?",
                "options": [{ "label": "Ship", "description": "Deploy now" }]
            }]
        }))
        .unwrap();

        let questions = normalize_openai_questions(args).unwrap();

        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].original_id.as_deref(), Some("q1"));
        assert_eq!(questions[0].question_type, QuestionType::MultipleChoice);
        assert!(questions[0].allow_freeform);
        assert_eq!(questions[0].text, "Decision\n\nWhich path?");
        assert_eq!(questions[0].options[0].key, "option_1");
        assert_eq!(questions[0].options[0].label, "Ship");
        assert_eq!(
            questions[0].options[0].description.as_deref(),
            Some("Deploy now")
        );
    }

    #[test]
    fn anthropic_multiselect_preserves_preview_and_formats_comma_joined_answers() {
        let args: AnthropicQuestionToolArgs = serde_json::from_value(json!({
            "questions": [{
                "header": "Pick features",
                "question": "Which features?",
                "multiSelect": true,
                "options": [{
                    "label": "Auth",
                    "description": "Login support",
                    "preview": "auth diff"
                }]
            }]
        }))
        .unwrap();

        let questions = normalize_anthropic_questions(args, &ANTHROPIC_QUESTION_LIMITS).unwrap();

        assert_eq!(questions[0].question_type, QuestionType::MultiSelect);
        assert_eq!(
            questions[0].options[0].preview.as_deref(),
            Some("auth diff")
        );
        let text =
            format_anthropic_answers(&[answered(None, "Which features?", &["Auth", "Billing"])])
                .unwrap();
        assert!(text.contains("\"Which features?\"=\"Auth, Billing\""));
    }

    #[test]
    fn openai_answers_are_keyed_by_original_model_question_id() {
        let text = format_openai_answers(&[
            answered(Some("first"), "First?", &["Yes"]),
            answered(Some("second"), "Second?", &["No"]),
        ])
        .unwrap();

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!({
                "answers": {
                    "first": { "answers": ["Yes"] },
                    "second": { "answers": ["No"] }
                }
            })
        );
    }

    #[test]
    fn option_description_and_preview_are_bounded() {
        let long = "x".repeat(OPTION_PREVIEW_MAX_CHARS + 10);

        assert_eq!(
            bounded_display_field(&long, OPTION_DESCRIPTION_MAX_CHARS)
                .chars()
                .count(),
            OPTION_DESCRIPTION_MAX_CHARS
        );
        assert_eq!(
            bounded_display_field(&long, OPTION_PREVIEW_MAX_CHARS)
                .chars()
                .count(),
            OPTION_PREVIEW_MAX_CHARS
        );
    }

    #[test]
    fn question_tool_registration_is_profile_specific() {
        let mut openai = ToolRegistry::new();
        register_question_tools(AgentProfileKind::OpenAi, &mut openai);
        assert!(openai.get(OPENAI_REQUEST_USER_INPUT_TOOL).is_some());
        assert!(openai.get(ANTHROPIC_ASK_USER_QUESTION_TOOL).is_none());

        let mut gpt56 = ToolRegistry::with_vocabulary(ToolVocabulary::Codex);
        register_question_tools(AgentProfileKind::Gpt56, &mut gpt56);
        assert!(gpt56.get(OPENAI_REQUEST_USER_INPUT_TOOL).is_some());
        assert!(gpt56.get(ANTHROPIC_ASK_USER_QUESTION_TOOL).is_none());

        let mut anthropic = ToolRegistry::new();
        register_question_tools(AgentProfileKind::Anthropic, &mut anthropic);
        assert!(anthropic.get(ANTHROPIC_ASK_USER_QUESTION_TOOL).is_some());
        assert!(anthropic.get(OPENAI_REQUEST_USER_INPUT_TOOL).is_none());

        let mut kimi = ToolRegistry::new();
        register_question_tools(AgentProfileKind::Kimi, &mut kimi);
        assert!(kimi.get(ANTHROPIC_ASK_USER_QUESTION_TOOL).is_some());
        assert!(kimi.get(OPENAI_REQUEST_USER_INPUT_TOOL).is_none());

        let mut claude5 = ToolRegistry::with_vocabulary(ToolVocabulary::Claude5);
        register_question_tools(AgentProfileKind::Claude5, &mut claude5);
        let tool = claude5.get(ANTHROPIC_ASK_USER_QUESTION_TOOL).unwrap();
        assert_eq!(tool.definition.parameters["additionalProperties"], false);
        assert_eq!(
            tool.definition.parameters["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["questions"]
        );
        assert_eq!(
            tool.definition.parameters["properties"]["questions"]["maxItems"],
            4
        );
        assert!(claude5.get(OPENAI_REQUEST_USER_INPUT_TOOL).is_none());

        let mut gemini = ToolRegistry::new();
        register_question_tools(AgentProfileKind::Gemini, &mut gemini);
        assert!(gemini.names().is_empty());
    }

    #[test]
    fn claude5_question_contract_is_strict_and_preserves_preview() {
        let args: AnthropicQuestionToolArgs = serde_json::from_value(json!({
            "questions": [{
                "header": "Approach",
                "question": "Which approach should we use?",
                "multiSelect": false,
                "options": [
                    {
                        "label": "Simple",
                        "description": "Use the smallest implementation.",
                        "preview": "fn simple() {}"
                    },
                    {
                        "label": "Flexible",
                        "description": "Allow future extension."
                    }
                ]
            }]
        }))
        .unwrap();

        let questions = normalize_anthropic_questions(args, &CLAUDE5_QUESTION_LIMITS).unwrap();

        assert_eq!(questions[0].header.as_deref(), Some("Approach"));
        assert_eq!(
            questions[0].options[0].preview.as_deref(),
            Some("fn simple() {}")
        );
        assert!(questions[0].allow_freeform);
    }

    /// The Claude 5 payload is deserialized through the lenient struct now, so
    /// the rules its own struct used to enforce are the normalizer's job.
    #[test]
    fn claude5_limits_reject_what_the_lenient_contract_allows() {
        let question = |patch: serde_json::Value| {
            let mut base = json!({
                "question": "Which approach?",
                "header": "Approach",
                "multiSelect": false,
                "options": [
                    {"label": "First", "description": "One"},
                    {"label": "Second", "description": "Two"}
                ]
            });
            let object = base.as_object_mut().unwrap();
            for (key, value) in patch.as_object().unwrap() {
                if value.is_null() {
                    object.remove(key);
                } else {
                    object.insert(key.clone(), value.clone());
                }
            }
            base
        };
        let normalize = |questions: serde_json::Value| {
            let args: AnthropicQuestionToolArgs =
                serde_json::from_value(json!({"questions": questions})).unwrap();
            normalize_anthropic_questions(args, &CLAUDE5_QUESTION_LIMITS)
        };

        // A missing header and a missing option description used to be caught
        // by serde; the normalizer has to reject them now.
        assert!(normalize(json!([question(json!({"header": null}))])).is_err());
        assert!(
            normalize(json!([question(json!({
                "options": [{"label": "First"}, {"label": "Second"}]
            }))]))
            .is_err()
        );

        assert!(
            normalize(json!([question(json!({"header": "ThirteenChars"}))])).is_err(),
            "header longer than 12 characters"
        );
        assert!(
            normalize(json!([question(json!({
                "options": [{"label": "Only", "description": "One"}]
            }))]))
            .is_err(),
            "fewer than two options"
        );
        assert!(
            normalize(json!(vec![question(json!({})); 5])).is_err(),
            "more than four questions"
        );

        assert!(normalize(json!([question(json!({}))])).is_ok());
    }

    /// The same payloads stay acceptable under the lenient contract, so the
    /// shared normalizer has not tightened the Anthropic tool.
    #[test]
    fn anthropic_limits_still_accept_optional_headers_and_descriptions() {
        let args: AnthropicQuestionToolArgs = serde_json::from_value(json!({
            "questions": [{
                "question": "Which approach?",
                "options": [{"label": "First"}]
            }]
        }))
        .unwrap();

        let questions = normalize_anthropic_questions(args, &ANTHROPIC_QUESTION_LIMITS).unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].header, None);
        assert_eq!(questions[0].options[0].description, None);
    }

    #[test]
    fn claude5_rejects_previews_for_multi_select_questions() {
        let args: AnthropicQuestionToolArgs = serde_json::from_value(json!({
            "questions": [{
                "header": "Features",
                "question": "Which features should we enable?",
                "multiSelect": true,
                "options": [
                    {
                        "label": "Auth",
                        "description": "Enable authentication.",
                        "preview": "auth = true"
                    },
                    {
                        "label": "Metrics",
                        "description": "Enable metrics."
                    }
                ]
            }]
        }))
        .unwrap();

        assert!(normalize_anthropic_questions(args, &CLAUDE5_QUESTION_LIMITS).is_err());
    }

    #[tokio::test]
    async fn claude5_question_tool_rejects_subagent_sessions() {
        let tool = make_claude5_question_tool();
        let error = (tool.executor)(
            json!({
                "questions": [{
                    "header": "Approach",
                    "question": "Which approach?",
                    "multiSelect": false,
                    "options": [
                        {
                            "label": "Simple",
                            "description": "Use the simple approach."
                        },
                        {
                            "label": "Flexible",
                            "description": "Use the flexible approach."
                        }
                    ]
                }]
            }),
            ToolContext {
                write_locks:         None,
                env:                 Arc::new(MockSandbox::default()),
                cancel:              CancellationToken::new(),
                tool_env_provider:   None,
                session_id:          Some("child".to_string()),
                root_session_id:     Some("root".to_string()),
                tool_call_id:        Some("call".to_string()),
                agent_event_emitter: None,
            },
        )
        .await
        .unwrap_err();

        assert!(error.contains("only available to the root agent"));
    }
}
