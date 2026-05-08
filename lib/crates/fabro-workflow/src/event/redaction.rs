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
    use ::fabro_types::{fixtures, run_event as fabro_types};

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
}
