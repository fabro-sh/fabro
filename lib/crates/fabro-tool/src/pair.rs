use fabro_types::{MAX_PAIR_MESSAGE_BYTES, PairId, PairMessageRequest, StageId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strum::IntoStaticStr;

use super::common::{FabroToolBackend, ToolError, ToolResult};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RunPairAction {
    Status,
    Start,
    Get,
    Message,
    End,
    Transcript,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabroRunPairParams {
    pub action:            RunPairAction,
    pub run_id:            Option<String>,
    pub pair_id:           Option<String>,
    pub stage_id:          Option<String>,
    pub text:              Option<String>,
    pub client_message_id: Option<String>,
    pub since_seq:         Option<u32>,
    pub limit:             Option<u32>,
}

#[derive(Debug)]
pub struct ValidatedPairRun {
    pub run_id: String,
    pub action: ValidatedPairAction,
}

#[derive(Debug)]
pub enum ValidatedPairAction {
    Status,
    Start {
        stage_id: StageId,
    },
    Get {
        pair_id: PairId,
    },
    Message {
        pair_id:           PairId,
        text:              String,
        client_message_id: Option<String>,
    },
    End {
        pair_id: PairId,
    },
    Transcript {
        pair_id:   PairId,
        since_seq: Option<u32>,
        limit:     Option<u32>,
    },
}

impl ValidatedPairAction {
    fn action(&self) -> RunPairAction {
        match self {
            Self::Status => RunPairAction::Status,
            Self::Start { .. } => RunPairAction::Start,
            Self::Get { .. } => RunPairAction::Get,
            Self::Message { .. } => RunPairAction::Message,
            Self::End { .. } => RunPairAction::End,
            Self::Transcript { .. } => RunPairAction::Transcript,
        }
    }
}

impl TryFrom<FabroRunPairParams> for ValidatedPairRun {
    type Error = ToolError;

    fn try_from(params: FabroRunPairParams) -> Result<Self, Self::Error> {
        let Some(run_id) = params
            .run_id
            .as_deref()
            .map(str::trim)
            .filter(|run_id| !run_id.is_empty())
        else {
            return Err(ToolError::message("run_id is required"));
        };
        let run_id = run_id.to_string();

        let action = match params.action {
            RunPairAction::Status => ValidatedPairAction::Status,
            RunPairAction::Start => {
                let Some(stage_id_raw) = params
                    .stage_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|stage_id| !stage_id.is_empty())
                else {
                    return Err(ToolError::message("stage_id is required for action start"));
                };
                let stage_id = stage_id_raw.parse::<StageId>().map_err(|err| {
                    ToolError::message(format!("invalid stage_id for action start: {err}"))
                })?;
                ValidatedPairAction::Start { stage_id }
            }
            RunPairAction::Get => {
                let pair_id =
                    parse_pair_id_for_action(params.pair_id.as_deref(), RunPairAction::Get)?;
                ValidatedPairAction::Get { pair_id }
            }
            RunPairAction::Message => {
                let pair_id =
                    parse_pair_id_for_action(params.pair_id.as_deref(), RunPairAction::Message)?;
                let Some(text) = params
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                else {
                    return Err(ToolError::message("text is required for action message"));
                };
                if text.len() > MAX_PAIR_MESSAGE_BYTES {
                    return Err(ToolError::message(format!(
                        "text must be at most {MAX_PAIR_MESSAGE_BYTES} bytes for action message"
                    )));
                }
                ValidatedPairAction::Message {
                    pair_id,
                    text: text.to_string(),
                    client_message_id: params
                        .client_message_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string),
                }
            }
            RunPairAction::End => {
                let pair_id =
                    parse_pair_id_for_action(params.pair_id.as_deref(), RunPairAction::End)?;
                ValidatedPairAction::End { pair_id }
            }
            RunPairAction::Transcript => {
                let pair_id =
                    parse_pair_id_for_action(params.pair_id.as_deref(), RunPairAction::Transcript)?;
                ValidatedPairAction::Transcript {
                    pair_id,
                    since_seq: params.since_seq,
                    limit: params.limit,
                }
            }
        };
        Ok(Self { run_id, action })
    }
}

fn parse_pair_id_for_action(raw: Option<&str>, action: RunPairAction) -> ToolResult<PairId> {
    let name: &'static str = action.into();
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(ToolError::message(format!(
            "pair_id is required for action {name}"
        )));
    };
    raw.parse::<PairId>()
        .map_err(|err| ToolError::message(format!("invalid pair_id for action {name}: {err}")))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PairRunResult {
    pub run_id: String,
    pub action: RunPairAction,
    pub result: Value,
}

pub async fn pair_run(
    backend: std::sync::Arc<dyn FabroToolBackend>,
    params: ValidatedPairRun,
) -> ToolResult<PairRunResult> {
    let run_id = backend
        .resolve_run(&params.run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?
        .id;

    let action = params.action.action();
    let result = match params.action {
        ValidatedPairAction::Status => json!(
            backend
                .get_run_pair_status(&run_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        ),
        ValidatedPairAction::Start { stage_id } => json!(
            backend
                .start_run_pair(&run_id, stage_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        ),
        ValidatedPairAction::Get { pair_id } => json!(
            backend
                .get_run_pair(&run_id, &pair_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        ),
        ValidatedPairAction::Message {
            pair_id,
            text,
            client_message_id,
        } => json!(
            backend
                .send_run_pair_message(&run_id, &pair_id, PairMessageRequest {
                    text,
                    client_message_id,
                },)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        ),
        ValidatedPairAction::End { pair_id } => json!(
            backend
                .end_run_pair(&run_id, &pair_id)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        ),
        ValidatedPairAction::Transcript {
            pair_id,
            since_seq,
            limit,
        } => json!(
            backend
                .get_run_pair_transcript(&run_id, &pair_id, since_seq, limit)
                .await
                .map_err(|err| ToolError::from_anyhow(&err))?
        ),
    };

    Ok(PairRunResult {
        run_id: run_id.to_string(),
        action,
        result,
    })
}

pub fn pair_run_text(result: &PairRunResult) -> String {
    match result.action {
        RunPairAction::Status => format!("read pair status for Fabro run {}", result.run_id),
        RunPairAction::Start => format!("started pair for Fabro run {}", result.run_id),
        RunPairAction::Get => format!("read pair for Fabro run {}", result.run_id),
        RunPairAction::Message => format!("sent pair message for Fabro run {}", result.run_id),
        RunPairAction::End => format!("ended pair for Fabro run {}", result.run_id),
        RunPairAction::Transcript => {
            format!("read pair transcript for Fabro run {}", result.run_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use fabro_types::{
        PairId, PairMessageId, PairMessageRecord, PairRecord, PairStatus, PairTarget,
        PairTranscriptAssistantMessage, PairTranscriptEntry, PairTranscriptMeta,
        PairTranscriptResponse, RunId, RunPairStatusResponse,
    };
    use serde_json::json;

    use super::*;

    fn run_id() -> RunId {
        RunId::new()
    }

    fn pair_id() -> PairId {
        "01HZX6M29F1CD5YYMHT1F5D7WQ".parse().unwrap()
    }

    fn pair_target() -> PairTarget {
        PairTarget {
            stage_id:   "code@1".parse().unwrap(),
            node_label: "Code".to_string(),
        }
    }

    fn assert_no_public_pair_leaks(value: &serde_json::Value) {
        let text = value.to_string();
        assert!(!text.contains("agent_session_id"), "{text}");
        assert!(!text.contains("session_id"), "{text}");
        assert!(!text.contains("provider"), "{text}");
        assert!(!text.contains("\"model\""), "{text}");
        assert!(!text.contains("\"node_id\""), "{text}");
        assert!(!text.contains("\"visit\""), "{text}");
    }

    #[test]
    fn missing_or_blank_run_id_returns_tool_error() {
        for raw in [None, Some(String::new()), Some("   ".to_string())] {
            let err = ValidatedPairRun::try_from(FabroRunPairParams {
                action:            RunPairAction::Status,
                run_id:            raw,
                pair_id:           None,
                stage_id:          None,
                text:              None,
                client_message_id: None,
                since_seq:         None,
                limit:             None,
            })
            .unwrap_err();
            assert!(
                err.as_str().contains("run_id is required"),
                "{}",
                err.as_str()
            );
        }
    }

    #[test]
    fn start_requires_stage_id() {
        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Start,
            run_id:            Some("run_123".to_string()),
            pair_id:           None,
            stage_id:          None,
            text:              None,
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str()
                .contains("stage_id is required for action start"),
            "{}",
            err.as_str()
        );

        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Start,
            run_id:            Some("run_123".to_string()),
            pair_id:           None,
            stage_id:          Some("bad-stage-id".to_string()),
            text:              None,
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str().contains("invalid stage_id for action start"),
            "{}",
            err.as_str()
        );
    }

    #[test]
    fn message_requires_pair_id_and_text() {
        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Message,
            run_id:            Some("run_123".to_string()),
            pair_id:           None,
            stage_id:          None,
            text:              Some("hello".to_string()),
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str()
                .contains("pair_id is required for action message"),
            "{}",
            err.as_str()
        );

        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Message,
            run_id:            Some("run_123".to_string()),
            pair_id:           Some(pair_id().to_string()),
            stage_id:          None,
            text:              None,
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str().contains("text is required for action message"),
            "{}",
            err.as_str()
        );
    }

    #[test]
    fn message_rejects_overlong_text() {
        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Message,
            run_id:            Some("run_123".to_string()),
            pair_id:           Some(pair_id().to_string()),
            stage_id:          None,
            text:              Some("a".repeat(MAX_PAIR_MESSAGE_BYTES + 1)),
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str()
                .contains("text must be at most 8192 bytes for action message"),
            "{}",
            err.as_str()
        );
    }

    #[test]
    fn transcript_requires_pair_id() {
        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Transcript,
            run_id:            Some("run_123".to_string()),
            pair_id:           None,
            stage_id:          None,
            text:              None,
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str()
                .contains("pair_id is required for action transcript"),
            "{}",
            err.as_str()
        );
    }

    #[test]
    fn end_requires_pair_id() {
        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::End,
            run_id:            Some("run_123".to_string()),
            pair_id:           None,
            stage_id:          None,
            text:              None,
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str().contains("pair_id is required for action end"),
            "{}",
            err.as_str()
        );
    }

    #[test]
    fn invalid_pair_id_for_action_is_reported() {
        let err = ValidatedPairRun::try_from(FabroRunPairParams {
            action:            RunPairAction::Get,
            run_id:            Some("run_123".to_string()),
            pair_id:           Some("not-a-pair-id".to_string()),
            stage_id:          None,
            text:              None,
            client_message_id: None,
            since_seq:         None,
            limit:             None,
        })
        .unwrap_err();
        assert!(
            err.as_str().contains("invalid pair_id for action get"),
            "{}",
            err.as_str()
        );
    }

    #[test]
    fn pair_run_result_status_does_not_leak_internals() {
        let status = RunPairStatusResponse {
            run_id:       run_id(),
            current_pair: Some(PairRecord {
                pair_id:        pair_id(),
                run_id:         run_id(),
                status:         PairStatus::Active,
                started_at:     Utc::now(),
                ended_at:       None,
                failure_reason: None,
                target:         pair_target(),
            }),
            targets:      vec![pair_target()],
        };
        let result = PairRunResult {
            run_id: "run_123".to_string(),
            action: RunPairAction::Status,
            result: json!(status),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_no_public_pair_leaks(&value);
    }

    #[test]
    fn pair_run_result_start_does_not_leak_internals() {
        let record = PairRecord {
            pair_id:        pair_id(),
            run_id:         run_id(),
            status:         PairStatus::Active,
            started_at:     Utc::now(),
            ended_at:       None,
            failure_reason: None,
            target:         pair_target(),
        };
        let result = PairRunResult {
            run_id: "run_123".to_string(),
            action: RunPairAction::Start,
            result: json!(record),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_no_public_pair_leaks(&value);
    }

    #[test]
    fn pair_run_result_message_does_not_leak_internals() {
        let record = PairMessageRecord {
            message_id:        PairMessageId::new(),
            client_message_id: Some("c-1".to_string()),
            pair_id:           pair_id(),
            run_id:            run_id(),
            stage_id:          "code@1".parse().unwrap(),
            text:              "hi".to_string(),
            accepted_at:       Utc::now(),
        };
        let result = PairRunResult {
            run_id: "run_123".to_string(),
            action: RunPairAction::Message,
            result: json!(record),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_no_public_pair_leaks(&value);
    }

    #[test]
    fn pair_run_result_transcript_does_not_leak_internals() {
        let response = PairTranscriptResponse {
            data: vec![PairTranscriptEntry::AssistantMessage(
                PairTranscriptAssistantMessage {
                    seq:             7,
                    event_id:        "evt".to_string(),
                    ts:              Utc::now(),
                    pair_id:         pair_id(),
                    target:          pair_target(),
                    text:            "hi".to_string(),
                    tool_call_count: 0,
                },
            )],
            meta: PairTranscriptMeta {
                next_since_seq: 8,
                has_more:       false,
            },
        };
        let result = PairRunResult {
            run_id: "run_123".to_string(),
            action: RunPairAction::Transcript,
            result: json!(response),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_no_public_pair_leaks(&value);
    }
}
