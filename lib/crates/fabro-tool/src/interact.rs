use std::borrow::Cow;
use std::sync::Arc;

use fabro_api::types;
use fabro_types::RunId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::common;
use super::common::{FabroToolBackend, ToolError, ToolResult};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunInteractAction {
    Get,
    Start,
    Message,
    /// Cancel the active steerable agent's current round and park it
    /// waiting for a later `message`. The run sits idle until you follow up
    /// with `message` or `cancel`. To redirect the agent, prefer `message`
    /// (optionally with `interrupt: true`).
    Interrupt,
    Cancel,
    Archive,
    Unarchive,
    LinkParent,
    UnlinkParent,
    GetQuestions,
    Answer,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabroRunInteractParams {
    pub action:      RunInteractAction,
    pub run_id:      String,
    pub parent_id:   Option<String>,
    pub message:     Option<String>,
    pub interrupt:   Option<bool>,
    pub question_id: Option<String>,
    pub answer:      Option<AnswerValue>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct AnswerValue(Value);

impl From<Value> for AnswerValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl AnswerValue {
    pub(crate) fn into_inner(self) -> Value {
        self.0
    }
}

impl JsonSchema for AnswerValue {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "AnswerValue".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Answer payload for a pending Fabro question. Use a boolean for yes/no, a string or {\"text\": \"...\"} for freeform text, {\"option\": \"key\"} for a single choice, or {\"options\": [\"key\"]} for multi-select.",
            "anyOf": [
                { "type": "boolean" },
                { "type": "string" },
                {
                    "type": "object",
                    "properties": {
                        "option": { "type": "string" }
                    },
                    "required": ["option"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "options": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["options"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" }
                    },
                    "required": ["text"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

#[derive(Debug)]
pub struct ValidatedInteractRun {
    pub run_id: String,
    pub action: ValidatedInteractAction,
}

#[derive(Debug)]
pub enum ValidatedInteractAction {
    Get,
    Start,
    Message {
        message:   String,
        interrupt: bool,
    },
    Interrupt,
    Cancel,
    Archive,
    Unarchive,
    LinkParent {
        parent_id: String,
    },
    UnlinkParent,
    GetQuestions,
    Answer {
        question_id: String,
        body:        types::SubmitAnswerRequest,
    },
}

impl ValidatedInteractAction {
    fn action(&self) -> RunInteractAction {
        match self {
            Self::Get => RunInteractAction::Get,
            Self::Start => RunInteractAction::Start,
            Self::Message { .. } => RunInteractAction::Message,
            Self::Interrupt => RunInteractAction::Interrupt,
            Self::Cancel => RunInteractAction::Cancel,
            Self::Archive => RunInteractAction::Archive,
            Self::Unarchive => RunInteractAction::Unarchive,
            Self::LinkParent { .. } => RunInteractAction::LinkParent,
            Self::UnlinkParent => RunInteractAction::UnlinkParent,
            Self::GetQuestions => RunInteractAction::GetQuestions,
            Self::Answer { .. } => RunInteractAction::Answer,
        }
    }
}

impl TryFrom<FabroRunInteractParams> for ValidatedInteractRun {
    type Error = ToolError;

    fn try_from(params: FabroRunInteractParams) -> Result<Self, Self::Error> {
        if params.run_id.trim().is_empty() {
            return Err(ToolError::message("run_id is required"));
        }
        let action = match params.action {
            RunInteractAction::Get => ValidatedInteractAction::Get,
            RunInteractAction::Start => ValidatedInteractAction::Start,
            RunInteractAction::Message => {
                let Some(message) = params
                    .message
                    .as_deref()
                    .map(str::trim)
                    .filter(|message| !message.is_empty())
                else {
                    return Err(ToolError::message("message is required for action message"));
                };
                ValidatedInteractAction::Message {
                    message:   message.to_string(),
                    interrupt: params.interrupt.unwrap_or(false),
                }
            }
            RunInteractAction::Interrupt => ValidatedInteractAction::Interrupt,
            RunInteractAction::Cancel => ValidatedInteractAction::Cancel,
            RunInteractAction::Archive => ValidatedInteractAction::Archive,
            RunInteractAction::Unarchive => ValidatedInteractAction::Unarchive,
            RunInteractAction::LinkParent => {
                let Some(parent_id) = params
                    .parent_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|parent_id| !parent_id.is_empty())
                else {
                    return Err(ToolError::message(
                        "parent_id is required for action link_parent",
                    ));
                };
                ValidatedInteractAction::LinkParent {
                    parent_id: parent_id.to_string(),
                }
            }
            RunInteractAction::UnlinkParent => ValidatedInteractAction::UnlinkParent,
            RunInteractAction::GetQuestions => ValidatedInteractAction::GetQuestions,
            RunInteractAction::Answer => {
                let Some(question_id) = params
                    .question_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|question_id| !question_id.is_empty())
                else {
                    return Err(ToolError::message(
                        "question_id is required for action answer",
                    ));
                };
                let Some(answer) = params.answer else {
                    return Err(ToolError::message("answer is required for action answer"));
                };
                ValidatedInteractAction::Answer {
                    question_id: question_id.to_string(),
                    body:        answer_to_submit_request(answer.into_inner())?,
                }
            }
        };
        Ok(Self {
            run_id: params.run_id.trim().to_string(),
            action,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InteractRunResult {
    pub run_id: String,
    pub action: RunInteractAction,
    pub result: Value,
}

pub async fn interact_run(
    backend: Arc<dyn FabroToolBackend>,
    params: ValidatedInteractRun,
) -> ToolResult<InteractRunResult> {
    let run_id = backend
        .resolve_run(&params.run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?
        .id;
    let action = params.action.action();
    let result = match params.action {
        ValidatedInteractAction::Get => interact_get(backend.as_ref(), &run_id).await?,
        ValidatedInteractAction::Start => {
            let summary = backend
                .start_run(&run_id, false)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Message { message, interrupt } => {
            backend
                .steer_run(&run_id, message.clone(), interrupt)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "message": message, "interrupt": interrupt })
        }
        ValidatedInteractAction::Interrupt => {
            backend
                .interrupt_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "interrupted": true })
        }
        ValidatedInteractAction::Cancel => {
            let summary = backend
                .cancel_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Archive => {
            let summary = backend
                .archive_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Unarchive => {
            let summary = backend
                .unarchive_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::LinkParent { parent_id } => {
            let parent_id = backend
                .resolve_run(&parent_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
                .id;
            let summary = backend
                .link_run_parent(&run_id, &parent_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::UnlinkParent => {
            let summary = backend
                .unlink_run_parent(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::GetQuestions => {
            let questions = backend
                .list_run_questions(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "questions": questions })
        }
        ValidatedInteractAction::Answer { question_id, body } => {
            backend
                .submit_run_answer(&run_id, &question_id, body)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "question_id": question_id, "submitted": true })
        }
    };

    Ok(InteractRunResult {
        run_id: run_id.to_string(),
        action,
        result,
    })
}

pub fn interact_run_text(result: &InteractRunResult) -> String {
    format!(
        "completed {:?} for Fabro run {}",
        result.action, result.run_id
    )
}

async fn interact_get(backend: &dyn FabroToolBackend, run_id: &RunId) -> ToolResult<Value> {
    let summary = common::retrieve_run(backend, run_id).await?;
    let projection = backend
        .get_run_state(run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?;
    Ok(json!({
        "summary": common::run_summary_result(&summary),
        "projection": projection,
    }))
}

fn answer_to_submit_request(answer: Value) -> ToolResult<types::SubmitAnswerRequest> {
    match answer {
        Value::Bool(true) => Ok(types::SubmitAnswerYesRequest {
            kind: types::SubmitAnswerYesRequestKind::Yes,
        }
        .into()),
        Value::Bool(false) => Ok(types::SubmitAnswerNoRequest {
            kind: types::SubmitAnswerNoRequestKind::No,
        }
        .into()),
        Value::String(text) => Ok(text_answer_request(text)),
        Value::Object(mut object) => {
            if let Some(option) = object.remove("option") {
                let option_key = serde_json::from_value::<String>(option).map_err(|err| {
                    ToolError::message(format!("answer option must be a string: {err}"))
                })?;
                Ok(types::SubmitAnswerSelectedRequest {
                    kind: types::SubmitAnswerSelectedRequestKind::Selected,
                    option_key,
                }
                .into())
            } else if let Some(options) = object.remove("options") {
                let option_keys =
                    serde_json::from_value::<Vec<String>>(options).map_err(|err| {
                        ToolError::message(format!("answer options must be strings: {err}"))
                    })?;
                Ok(types::SubmitAnswerMultiSelectedRequest {
                    kind: types::SubmitAnswerMultiSelectedRequestKind::MultiSelected,
                    option_keys,
                }
                .into())
            } else if let Some(text) = object.remove("text") {
                let text = serde_json::from_value::<String>(text).map_err(|err| {
                    ToolError::message(format!("answer text must be a string: {err}"))
                })?;
                Ok(text_answer_request(text))
            } else {
                Err(ToolError::message(
                    "answer object must contain one of: option, options, text",
                ))
            }
        }
        other => Err(ToolError::message(format!(
            "unsupported answer value: {other}; expected boolean, string, or object",
        ))),
    }
}

fn text_answer_request(text: String) -> types::SubmitAnswerRequest {
    types::SubmitAnswerTextRequest {
        kind: types::SubmitAnswerTextRequestKind::Text,
        text,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn answer_payloads_map_to_submit_answer_wire_json() {
        let cases = [
            (json!(true), json!({ "kind": "yes" })),
            (json!(false), json!({ "kind": "no" })),
            (json!("hello"), json!({ "kind": "text", "text": "hello" })),
            (
                json!({ "option": "a" }),
                json!({ "kind": "selected", "option_key": "a" }),
            ),
            (
                json!({ "options": ["a", "b"] }),
                json!({ "kind": "multi_selected", "option_keys": ["a", "b"] }),
            ),
            (
                json!({ "text": "hello" }),
                json!({ "kind": "text", "text": "hello" }),
            ),
        ];

        for (answer, expected) in cases {
            let request = answer_to_submit_request(answer).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), expected);
        }
    }

    #[test]
    fn unsupported_answer_object_is_rejected() {
        let err = answer_to_submit_request(json!({ "value": "yes" })).unwrap_err();

        assert!(err.as_str().contains("option, options, text"));
    }

    #[test]
    fn interact_answer_validation_rejects_unsupported_json_before_api_calls() {
        let err = ValidatedInteractRun::try_from(FabroRunInteractParams {
            action:      RunInteractAction::Answer,
            run_id:      "run_123".to_string(),
            parent_id:   None,
            message:     None,
            interrupt:   None,
            question_id: Some("question-1".to_string()),
            answer:      Some(json!({ "value": "yes" }).into()),
        })
        .unwrap_err();

        assert!(err.as_str().contains("option, options, text"));
    }

    #[test]
    fn interact_link_parent_validation_rejects_missing_or_blank_parent_id() {
        for parent_id in [None, Some("   ".to_string())] {
            let err = ValidatedInteractRun::try_from(FabroRunInteractParams {
                action: RunInteractAction::LinkParent,
                run_id: "child-run".to_string(),
                parent_id,
                message: None,
                interrupt: None,
                question_id: None,
                answer: None,
            })
            .unwrap_err();

            assert!(
                err.as_str()
                    .contains("parent_id is required for action link_parent"),
                "{}",
                err.as_str()
            );
        }
    }

    #[test]
    fn interact_unlink_parent_validation_does_not_require_parent_id() {
        let validated = ValidatedInteractRun::try_from(FabroRunInteractParams {
            action:      RunInteractAction::UnlinkParent,
            run_id:      " child-run ".to_string(),
            parent_id:   None,
            message:     None,
            interrupt:   None,
            question_id: None,
            answer:      None,
        })
        .expect("unlink_parent should not require parent_id");

        assert_eq!(validated.run_id, "child-run");
        assert!(matches!(
            validated.action,
            ValidatedInteractAction::UnlinkParent
        ));
    }

    #[test]
    fn interrupt_action_requires_only_run_id() {
        let validated = ValidatedInteractRun::try_from(FabroRunInteractParams {
            action:      RunInteractAction::Interrupt,
            run_id:      "run_123".to_string(),
            parent_id:   None,
            message:     None,
            interrupt:   None,
            question_id: None,
            answer:      None,
        })
        .expect("interrupt should validate with only run_id");

        assert_eq!(validated.run_id, "run_123");
        assert!(matches!(
            validated.action,
            ValidatedInteractAction::Interrupt
        ));
    }
}
