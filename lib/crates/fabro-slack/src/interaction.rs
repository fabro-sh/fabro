use fabro_interview::Answer;
use serde_json::Value;

use crate::payload::{SlackActionPayload, SlackAnswerSubmission};

const MULTI_SELECT_BLOCK_ID: &str = "interview.checkboxes";
const MULTI_SELECT_ACTION_ID: &str = "interview.select";
const ANSWER_ACTION_ID: &str = "interview.answer";
const MULTI_SELECT_SUBMIT_ACTION_ID: &str = "interview.submit";

/// Parses a Slack interaction payload and returns a server-routable answer
/// submission.
pub fn parse_interaction(payload: &Value) -> Option<SlackAnswerSubmission> {
    if payload["type"].as_str()? != "block_actions" {
        return None;
    }

    let action = payload["actions"].as_array()?.first()?;
    let action_id = action["action_id"].as_str()?;
    let value = action["value"].as_str()?;
    let routed: SlackActionPayload = serde_json::from_str(value).ok()?;
    let question_ref = routed.question_ref();

    let action_type = action["type"].as_str().unwrap_or("button");

    let answer = match action_type {
        "button" if action_id == ANSWER_ACTION_ID => match routed {
            SlackActionPayload::Yes { .. } => Answer::yes(),
            SlackActionPayload::No { .. } => Answer::no(),
            SlackActionPayload::Selected { key, .. } => Answer {
                value:           fabro_interview::AnswerValue::Selected(key),
                selected_option: None,
                text:            None,
            },
            SlackActionPayload::SubmitMulti { .. } => return None,
        },
        "button" if action_id == MULTI_SELECT_SUBMIT_ACTION_ID => {
            extract_checkbox_selections(payload)
        }
        "checkboxes" => {
            // Ignore checkbox toggle events — wait for Submit button
            return None;
        }
        _ => return None,
    };

    Some(SlackAnswerSubmission {
        run_id: question_ref.run_id,
        qid: question_ref.qid,
        answer,
    })
}

/// Extract selected checkbox values from `payload.state.values`.
fn extract_checkbox_selections(payload: &Value) -> Answer {
    let selected =
        payload["state"]["values"][MULTI_SELECT_BLOCK_ID][MULTI_SELECT_ACTION_ID]["selected_options"]
            .as_array();

    match selected {
        Some(options) if !options.is_empty() => {
            let values: Vec<String> = options
                .iter()
                .filter_map(|opt| opt["value"].as_str().map(String::from))
                .collect();
            Answer::multi_selected(values)
        }
        _ => Answer::skipped(),
    }
}

#[cfg(test)]
mod tests {
    use fabro_interview::AnswerValue;

    use super::*;

    #[test]
    fn parse_yes_button_click() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.answer",
                "type": "button",
                "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
            }]
        });
        let result = parse_interaction(&payload).unwrap();
        assert_eq!(result.run_id, "run-1");
        assert_eq!(result.qid, "q-1");
        assert_eq!(result.answer.value, AnswerValue::Yes);
    }

    #[test]
    fn parse_no_button_click() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.answer",
                "type": "button",
                "value": "{\"kind\":\"no\",\"run_id\":\"run-1\",\"qid\":\"q-2\"}"
            }]
        });
        let result = parse_interaction(&payload).unwrap();
        assert_eq!(result.run_id, "run-1");
        assert_eq!(result.qid, "q-2");
        assert_eq!(result.answer.value, AnswerValue::No);
    }

    #[test]
    fn parse_multiple_choice_button() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.answer",
                "type": "button",
                "value": "{\"kind\":\"selected\",\"run_id\":\"run-1\",\"qid\":\"q-3\",\"key\":\"rs\"}"
            }]
        });
        let result = parse_interaction(&payload).unwrap();
        assert_eq!(result.qid, "q-3");
        assert_eq!(result.answer.value, AnswerValue::Selected("rs".to_string()));
    }

    #[test]
    fn checkbox_toggle_is_ignored() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.select",
                "type": "checkboxes",
                "selected_options": [
                    { "value": "a" },
                    { "value": "b" }
                ]
            }]
        });
        assert!(parse_interaction(&payload).is_none());
    }

    #[test]
    fn submit_button_reads_checkbox_state() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.submit",
                "type": "button",
                "value": "{\"kind\":\"submit_multi\",\"run_id\":\"run-1\",\"qid\":\"q-5\"}"
            }],
            "state": {
                "values": {
                    "interview.checkboxes": {
                        "interview.select": {
                            "type": "checkboxes",
                            "selected_options": [
                                { "value": "auth" },
                                { "value": "billing" }
                            ]
                        }
                    }
                }
            }
        });
        let result = parse_interaction(&payload).unwrap();
        assert_eq!(result.qid, "q-5");
        assert_eq!(
            result.answer.value,
            AnswerValue::MultiSelected(vec!["auth".to_string(), "billing".to_string()])
        );
    }

    #[test]
    fn submit_button_with_no_checkboxes_selected() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.submit",
                "type": "button",
                "value": "{\"kind\":\"submit_multi\",\"run_id\":\"run-1\",\"qid\":\"q-5\"}"
            }],
            "state": {
                "values": {
                    "interview.checkboxes": {
                        "interview.select": {
                            "type": "checkboxes",
                            "selected_options": []
                        }
                    }
                }
            }
        });
        let result = parse_interaction(&payload).unwrap();
        assert_eq!(result.qid, "q-5");
        assert_eq!(result.answer.value, AnswerValue::Skipped);
    }

    #[test]
    fn parse_plain_text_input() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "interview.answer",
                "type": "plain_text_input",
                "value": "{\"kind\":\"selected\",\"run_id\":\"run-1\",\"qid\":\"q-6\",\"key\":\"input\"}"
            }]
        });
        assert!(parse_interaction(&payload).is_none());
    }

    #[test]
    fn returns_none_for_empty_actions() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": []
        });
        assert!(parse_interaction(&payload).is_none());
    }

    #[test]
    fn returns_none_for_unknown_type() {
        let payload = serde_json::json!({
            "type": "view_submission"
        });
        assert!(parse_interaction(&payload).is_none());
    }

    #[test]
    fn returns_none_for_malformed_action_id() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "no-colon",
                "type": "button",
                "value": "yes"
            }]
        });
        assert!(parse_interaction(&payload).is_none());
    }
}
