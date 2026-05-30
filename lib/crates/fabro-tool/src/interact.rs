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
    Approve,
    Deny,
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
    pub reason:      Option<String>,
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
    Approve,
    Deny {
        reason: Option<String>,
    },
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
    /// Actions that may only be performed by a human user, never by a
    /// workflow-agent through its own `fabro_tools` MCP surface.
    pub fn requires_user(&self) -> bool {
        matches!(self, Self::Approve | Self::Deny { .. })
    }

    fn action(&self) -> RunInteractAction {
        match self {
            Self::Get => RunInteractAction::Get,
            Self::Start => RunInteractAction::Start,
            Self::Approve => RunInteractAction::Approve,
            Self::Deny { .. } => RunInteractAction::Deny,
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
        let reason = normalize_optional_text(params.reason.as_deref());
        if !matches!(params.action, RunInteractAction::Deny) && reason.is_some() {
            return Err(ToolError::message("reason is only valid for action deny"));
        }
        let action = match params.action {
            RunInteractAction::Get => ValidatedInteractAction::Get,
            RunInteractAction::Start => ValidatedInteractAction::Start,
            RunInteractAction::Approve => ValidatedInteractAction::Approve,
            RunInteractAction::Deny => ValidatedInteractAction::Deny { reason },
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
        ValidatedInteractAction::Approve => {
            let summary = backend
                .approve_run(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?;
            json!({ "summary": common::run_summary_result(&summary) })
        }
        ValidatedInteractAction::Deny { reason } => {
            let summary = backend
                .deny_run(&run_id, reason)
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

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use fabro_types::{
        EventEnvelope, FailureReason, Run, RunId, RunLifecycle, RunLinks, RunOrigin, RunProjection,
        RunStatus, RunTimestamps, WorkflowRef, test_support,
    };
    use serde_json::json;

    use super::*;

    fn interact_params(action: RunInteractAction) -> FabroRunInteractParams {
        FabroRunInteractParams {
            action,
            run_id: "run_123".to_string(),
            parent_id: None,
            message: None,
            interrupt: None,
            question_id: None,
            answer: None,
            reason: None,
        }
    }

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
            reason:      None,
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
                reason: None,
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
            reason:      None,
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
            reason:      None,
        })
        .expect("interrupt should validate with only run_id");

        assert_eq!(validated.run_id, "run_123");
        assert!(matches!(
            validated.action,
            ValidatedInteractAction::Interrupt
        ));
    }

    #[test]
    fn approve_action_requires_only_run_id() {
        let validated = ValidatedInteractRun::try_from(interact_params(RunInteractAction::Approve))
            .expect("approve should validate with only run_id");

        assert_eq!(validated.run_id, "run_123");
        assert!(matches!(validated.action, ValidatedInteractAction::Approve));
    }

    #[test]
    fn deny_action_normalizes_optional_reason() {
        for (raw_reason, expected) in [
            (None, None),
            (
                Some("  Needs review  ".to_string()),
                Some("Needs review".to_string()),
            ),
            (Some("   ".to_string()), None),
        ] {
            let mut params = interact_params(RunInteractAction::Deny);
            params.reason = raw_reason;

            let validated =
                ValidatedInteractRun::try_from(params).expect("deny should validate reason");

            match validated.action {
                ValidatedInteractAction::Deny { reason } => assert_eq!(reason, expected),
                other => panic!("expected deny action, got {other:?}"),
            }
        }
    }

    #[test]
    fn nonblank_reason_is_rejected_for_non_deny_actions() {
        let mut params = interact_params(RunInteractAction::Approve);
        params.reason = Some("because".to_string());

        let err = ValidatedInteractRun::try_from(params).unwrap_err();

        assert!(err.as_str().contains("reason"));
        assert!(err.as_str().contains("deny"));
    }

    #[tokio::test]
    async fn approve_dispatches_to_backend_and_returns_summary() {
        let run_id = run_id("01KRBZW5C00000000000000001");
        let backend = Arc::new(MockInteractBackend::new(run_id));

        let result = interact_run(backend.clone(), ValidatedInteractRun {
            run_id: "nightly".to_string(),
            action: ValidatedInteractAction::Approve,
        })
        .await
        .expect("approve should dispatch");

        assert!(matches!(result.action, RunInteractAction::Approve));
        assert_eq!(result.result["summary"]["run_id"], run_id.to_string());
        assert_eq!(backend.approved.lock().unwrap().as_slice(), &[run_id]);
    }

    #[tokio::test]
    async fn deny_dispatches_to_backend_with_reason_and_returns_summary() {
        let run_id = run_id("01KRBZW5C00000000000000001");
        let backend = Arc::new(MockInteractBackend::new(run_id));

        let result = interact_run(backend.clone(), ValidatedInteractRun {
            run_id: "nightly".to_string(),
            action: ValidatedInteractAction::Deny {
                reason: Some("Needs review".to_string()),
            },
        })
        .await
        .expect("deny should dispatch");

        assert!(matches!(result.action, RunInteractAction::Deny));
        assert_eq!(result.result["summary"]["run_id"], run_id.to_string());
        assert_eq!(backend.denied.lock().unwrap().as_slice(), &[(
            run_id,
            Some("Needs review".to_string())
        )]);
    }

    fn run_id(raw: &str) -> RunId {
        raw.parse().expect("test run id should parse")
    }

    fn run_with_status(run_id: RunId, status: RunStatus) -> Run {
        Run {
            id:               run_id,
            parent_id:        None,
            children_count:   0,
            title:            "Test run".to_string(),
            goal:             "Test run".to_string(),
            workflow:         WorkflowRef {
                slug:       Some("simple".to_string()),
                name:       Some("Simple".to_string()),
                graph_name: None,
                node_count: 0,
                edge_count: 0,
            },
            automation:       None,
            repository:       None,
            created_by:       test_support::test_principal(),
            origin:           RunOrigin::default(),
            labels:           HashMap::new(),
            lifecycle:        RunLifecycle {
                status,
                approval: None,
                pending_control: None,
                queue_position: None,
                error: None,
                archived: false,
                archived_at: None,
            },
            sandbox:          None,
            models:           Vec::new(),
            source_directory: None,
            timestamps:       RunTimestamps {
                created_at:    Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap(),
                started_at:    None,
                last_event_at: None,
                completed_at:  None,
            },
            timing:           None,
            billing:          None,
            size:             fabro_types::RunSize::default(),
            ask_fabro:        fabro_types::AskFabro::default(),
            diff:             None,
            pull_request:     None,
            current_question: None,
            superseded_by:    None,
            retried_from:     None,
            links:            RunLinks { web: None },
        }
    }

    struct MockInteractBackend {
        run_id:   RunId,
        approved: Mutex<Vec<RunId>>,
        denied:   Mutex<Vec<(RunId, Option<String>)>>,
    }

    impl MockInteractBackend {
        fn new(run_id: RunId) -> Self {
            Self {
                run_id,
                approved: Mutex::new(Vec::new()),
                denied: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl FabroToolBackend for MockInteractBackend {
        async fn create_run_from_spec(
            &self,
            _spec: &crate::ValidatedCreateRunSpec,
            _cwd: &Path,
            _user_settings_path: &Path,
            _parent_id: Option<RunId>,
        ) -> anyhow::Result<RunId> {
            unreachable!()
        }

        async fn resolve_run(&self, selector: &str) -> anyhow::Result<Run> {
            assert_eq!(selector, "nightly");
            Ok(run_with_status(self.run_id, RunStatus::Pending {
                reason: fabro_types::PendingReason::ApprovalRequired,
            }))
        }

        async fn retrieve_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn start_run(&self, _run_id: &RunId, _resume: bool) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn approve_run(&self, run_id: &RunId) -> anyhow::Result<Run> {
            self.approved.lock().unwrap().push(*run_id);
            Ok(run_with_status(*run_id, RunStatus::Runnable))
        }

        async fn deny_run(&self, run_id: &RunId, reason: Option<String>) -> anyhow::Result<Run> {
            self.denied.lock().unwrap().push((*run_id, reason));
            Ok(run_with_status(*run_id, RunStatus::Failed {
                reason: FailureReason::ApprovalDenied,
            }))
        }

        async fn cancel_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn interrupt_run(&self, _run_id: &RunId) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn steer_run(
            &self,
            _run_id: &RunId,
            _text: String,
            _interrupt: bool,
        ) -> anyhow::Result<()> {
            unreachable!()
        }

        async fn archive_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn unarchive_run(&self, _run_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn list_store_runs(&self) -> anyhow::Result<Vec<Run>> {
            unreachable!()
        }

        async fn list_store_runs_by_parent(&self, _parent_id: RunId) -> anyhow::Result<Vec<Run>> {
            unreachable!()
        }

        async fn link_run_parent(
            &self,
            _child_id: &RunId,
            _parent_id: &RunId,
        ) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn unlink_run_parent(&self, _child_id: &RunId) -> anyhow::Result<Run> {
            unreachable!()
        }

        async fn get_run_state(&self, _run_id: &RunId) -> anyhow::Result<RunProjection> {
            unreachable!()
        }

        async fn list_run_events(
            &self,
            _run_id: &RunId,
            _after: Option<u32>,
            _limit: Option<usize>,
        ) -> anyhow::Result<Vec<EventEnvelope>> {
            unreachable!()
        }

        async fn list_run_events_until(
            &self,
            _run_id: &RunId,
            _after: Option<u32>,
            _limit: usize,
        ) -> anyhow::Result<Vec<EventEnvelope>> {
            unreachable!()
        }

        async fn list_run_questions(
            &self,
            _run_id: &RunId,
        ) -> anyhow::Result<Vec<types::ApiQuestion>> {
            unreachable!()
        }

        async fn submit_run_answer(
            &self,
            _run_id: &RunId,
            _question_id: &str,
            _body: types::SubmitAnswerRequest,
        ) -> anyhow::Result<()> {
            unreachable!()
        }
    }
}
