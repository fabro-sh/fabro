use std::borrow::Cow;
use std::sync::Arc;

use fabro_api::types;
use fabro_client::Client;
use fabro_types::RunId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::common;
use super::common::{ToolError, ToolResult};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunInteractAction {
    Get,
    Start,
    Message,
    Cancel,
    Archive,
    Unarchive,
    GetQuestions,
    Answer,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct FabroRunInteractParams {
    pub(crate) action:      RunInteractAction,
    pub(crate) run_id:      String,
    pub(crate) message:     Option<String>,
    pub(crate) interrupt:   Option<bool>,
    pub(crate) question_id: Option<String>,
    pub(crate) answer:      Option<AnswerValue>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(crate) struct AnswerValue(Value);

impl From<Value> for AnswerValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl AnswerValue {
    fn into_inner(self) -> Value {
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
pub(crate) struct ValidatedInteractRun {
    pub(crate) run_id: String,
    pub(crate) action: ValidatedInteractAction,
}

#[derive(Debug)]
pub(crate) enum ValidatedInteractAction {
    Get,
    Start,
    Message {
        message:   String,
        interrupt: bool,
    },
    Cancel,
    Archive,
    Unarchive,
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
            Self::Cancel => RunInteractAction::Cancel,
            Self::Archive => RunInteractAction::Archive,
            Self::Unarchive => RunInteractAction::Unarchive,
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
            RunInteractAction::Cancel => ValidatedInteractAction::Cancel,
            RunInteractAction::Archive => ValidatedInteractAction::Archive,
            RunInteractAction::Unarchive => ValidatedInteractAction::Unarchive,
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
pub(crate) struct InteractRunResult {
    pub(crate) run_id: String,
    pub(crate) action: RunInteractAction,
    pub(crate) result: Value,
}

pub(crate) async fn interact_run(
    client: Arc<Client>,
    params: ValidatedInteractRun,
) -> ToolResult<InteractRunResult> {
    let run_id = client
        .resolve_run(&params.run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?
        .id;
    let action = params.action.action();
    let result = match params.action {
        ValidatedInteractAction::Get => interact_get(&client, &run_id).await?,
        ValidatedInteractAction::Start => {
            let summary = client
                .start_run(&run_id, false)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Message { message, interrupt } => {
            client
                .steer_run(&run_id, message.clone(), interrupt)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "message": message, "interrupt": interrupt })
        }
        ValidatedInteractAction::Cancel => {
            let summary = client
                .cancel_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Archive => {
            let summary = client
                .archive_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Unarchive => {
            let summary = client
                .unarchive_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::GetQuestions => {
            let questions = client
                .list_run_questions(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "questions": questions })
        }
        ValidatedInteractAction::Answer { question_id, body } => {
            client
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

pub(crate) fn interact_run_text(result: &InteractRunResult) -> String {
    format!(
        "completed {:?} for Fabro run {}",
        result.action, result.run_id
    )
}

async fn interact_get(client: &Client, run_id: &RunId) -> ToolResult<Value> {
    let summary = common::retrieve_run(client, run_id).await?;
    let projection = client
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
            message:     None,
            interrupt:   None,
            question_id: Some("question-1".to_string()),
            answer:      Some(json!({ "value": "yes" }).into()),
        })
        .unwrap_err();

        assert!(err.as_str().contains("option, options, text"));
    }
}
