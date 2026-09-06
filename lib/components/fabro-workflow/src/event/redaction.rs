use ::fabro_types::{RunEvent, RunId};
use anyhow::{Context, Result};
use fabro_redact::redact_json_value;
use fabro_store::EventPayload;
use fabro_util::json::normalize_json_value;
use serde_json::Value;

pub fn build_redacted_event_payload(event: &RunEvent, run_id: &RunId) -> Result<EventPayload> {
    let value = redacted_event_value(event)?;
    EventPayload::new(value, run_id).map_err(anyhow::Error::from)
}

pub fn redacted_event_json(event: &RunEvent) -> Result<String> {
    serde_json::to_string(&redacted_event_value(event)?).map_err(anyhow::Error::from)
}

fn normalized_event_value(event: &RunEvent) -> Result<Value> {
    let value = event.to_value()?;
    Ok(normalize_json_value(value))
}

fn redacted_event_value(event: &RunEvent) -> Result<Value> {
    Ok(redact_json_value(normalized_event_value(event)?))
}

pub fn event_payload_from_redacted_json(line: &str, run_id: &RunId) -> Result<EventPayload> {
    let value = serde_json::from_str(line).context("Failed to parse redacted event payload")?;
    EventPayload::new(value, run_id).map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use ::fabro_types::{ReasoningOutput, fixtures, run_event as fabro_types};
    use fabro_agent::{AgentEvent, SessionSpend};
    use fabro_llm::types::TokenCounts as LlmTokenCounts;
    use fabro_model::{ModelRef, ProviderId};

    use super::*;
    use crate::event::{Event, to_run_event};

    #[test]
    fn build_redacted_event_payload_requires_id() {
        let stored = to_run_event(&fixtures::RUN_8, &Event::RunSubmitted {
            definition_blob: None,
        });
        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_8).unwrap();
        assert_eq!(payload.as_value()["id"], stored.id);
        assert_eq!(payload.as_value()["event"], "run.submitted");
    }

    #[test]
    fn build_redacted_event_payload_redacts_exec_output_tail_values() {
        let secret = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";
        let stored = to_run_event(&fixtures::RUN_8, &Event::SetupFailed {
            command:          "setup".to_string(),
            index:            0,
            exit_code:        1,
            stderr:           "compat stderr".to_string(),
            exec_output_tail: Some(fabro_types::ExecOutputTail {
                stdout:           Some(format!("stdout {secret}")),
                stderr:           Some("plain stderr".to_string()),
                stdout_truncated: false,
                stderr_truncated: false,
            }),
        });

        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_8).unwrap();
        let payload_text = serde_json::to_string(payload.as_value()).unwrap();

        assert!(!payload_text.contains(secret));
        assert!(payload_text.contains("REDACTED"));
        assert_eq!(payload.as_value()["event"], "setup.failed");
        assert_eq!(
            payload.as_value()["properties"]["exec_output_tail"]["stderr"],
            "plain stderr"
        );
    }

    #[test]
    fn build_redacted_event_payload_redacts_tool_process_output_tails() {
        let secret = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";
        let stored = to_run_event(&fixtures::RUN_8, &Event::Agent {
            stage:             "code".to_string(),
            visit:             1,
            event:             AgentEvent::ToolProcessCompleted {
                exit_code:             Some(7),
                termination:           ::fabro_types::CommandTermination::Exited,
                duration_ms:           12,
                streams_separated:     true,
                output_bytes_observed: 100,
                output_bytes_retained: 100,
                output_bytes_omitted:  0,
                exec_output_tail:      Some(fabro_types::ExecOutputTail {
                    stdout:           Some(format!("stdout {secret}")),
                    stderr:           Some("plain stderr".to_string()),
                    stdout_truncated: false,
                    stderr_truncated: false,
                }),
            },
            session_id:        Some("ses_child".to_string()),
            parent_session_id: None,
            tool_call_id:      Some("call_1".to_string()),
        });

        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_8).unwrap();
        let payload_text = serde_json::to_string(payload.as_value()).unwrap();

        assert!(!payload_text.contains(secret));
        assert!(payload_text.contains("REDACTED"));
        assert_eq!(payload.as_value()["event"], "agent.tool.process.completed");
        assert_eq!(
            payload.as_value()["properties"]["exec_output_tail"]["stderr"],
            "plain stderr"
        );
    }

    /// Reasoning is model-authored text like any other, so it goes through
    /// the same canonical redaction pass as assistant output.
    #[test]
    fn build_redacted_event_payload_redacts_secrets_inside_reasoning() {
        let secret = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";
        let stored = to_run_event(&fixtures::RUN_8, &Event::Agent {
            stage:             "code".to_string(),
            visit:             1,
            event:             AgentEvent::AssistantMessage {
                text:            "done".to_string(),
                model:           ModelRef {
                    provider: ProviderId::openai(),
                    model_id: "gpt-5.4".into(),
                    speed:    None,
                },
                usage:           LlmTokenCounts::default(),
                cost_usd:        None,
                cost_source:     None,
                session_spend:   SessionSpend::default(),
                tool_call_count: 0,
                context_window:  None,
                reasoning:       Some(ReasoningOutput::new(
                    format!("the key is {secret}"),
                    format!("reading {secret} from the env"),
                )),
            },
            session_id:        Some("ses_agent".to_string()),
            parent_session_id: None,
            tool_call_id:      None,
        });

        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_8).unwrap();
        let reasoning = &payload.as_value()["properties"]["reasoning"];
        let summary = reasoning["summary"].as_str().unwrap();
        let trace = reasoning["trace"].as_str().unwrap();

        assert!(!summary.contains(secret));
        assert!(!trace.contains(secret));
        assert!(summary.contains("REDACTED"));
        assert!(trace.contains("REDACTED"));
    }
}
