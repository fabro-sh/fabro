use fabro_interview::Answer;
use serde_json::Value;

use crate::blocks::ANSWER_ACTION_ID_PREFIX;
use crate::payload::{self, SlackActionPayload, SlackAnswerSubmission};

const MULTI_SELECT_BLOCK_ID: &str = "interview.checkboxes";
const MULTI_SELECT_ACTION_ID: &str = "interview.select";
const MULTI_SELECT_SUBMIT_ACTION_ID: &str = "interview.submit";

/// Buttons for the same question must each have a unique `action_id`, so the
/// outbound side stamps `interview.answer.<suffix>` per element. This matches
/// either the exact prefix (legacy compatibility for messages posted before
/// the suffix scheme) or the suffixed form (current).
const ANSWER_ACTION_ID_PREFIX_DOT: &str = "interview.answer.";

fn is_answer_action(action_id: &str) -> bool {
    action_id == ANSWER_ACTION_ID_PREFIX || action_id.starts_with(ANSWER_ACTION_ID_PREFIX_DOT)
}

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
    let actor = payload::interaction_actor(payload)?;

    let action_type = action["type"].as_str().unwrap_or("button");

    let answer = match action_type {
        "button" if is_answer_action(action_id) => match routed {
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
        actor,
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
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
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
        assert_eq!(result.actor, fabro_types::Principal::Slack {
            team_id:   "T123".to_string(),
            user_id:   "U123".to_string(),
            user_name: Some("ada".to_string()),
        });
    }

    #[test]
    fn parse_no_button_click() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
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
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
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
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
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
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
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
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
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

    /// Suffixed `action_id`s (per-button uniqueness for Slack) must still
    /// route to the correct answer.
    #[test]
    fn parse_suffixed_yes_action_id() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
            "actions": [{
                "action_id": "interview.answer.yes",
                "type": "button",
                "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
            }]
        });
        let submission = parse_interaction(&payload).unwrap();
        assert_eq!(submission.answer.value, AnswerValue::Yes);
    }

    #[test]
    fn parse_suffixed_multiple_choice_action_id() {
        // `interview.answer.<index>` is what `question_to_blocks` now produces
        // for multiple_choice questions.
        let payload = serde_json::json!({
            "type": "block_actions",
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
            "actions": [{
                "action_id": "interview.answer.2",
                "type": "button",
                "value": "{\"kind\":\"selected\",\"run_id\":\"run-1\",\"qid\":\"q-1\",\"key\":\"py\"}"
            }]
        });
        let submission = parse_interaction(&payload).unwrap();
        assert_eq!(
            submission.answer.value,
            AnswerValue::Selected("py".to_string())
        );
    }

    /// Legacy `action_id` without a suffix must still parse so messages
    /// posted by older Fabro builds remain clickable after upgrade.
    #[test]
    fn parse_legacy_unsuffixed_action_id() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
            "actions": [{
                "action_id": "interview.answer",
                "type": "button",
                "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
            }]
        });
        let submission = parse_interaction(&payload).unwrap();
        assert_eq!(submission.answer.value, AnswerValue::Yes);
    }

    /// Action ids that merely share a prefix but are not the answer family
    /// must not be misrouted (no false-positive prefix match).
    #[test]
    fn rejects_lookalike_action_id() {
        let payload = serde_json::json!({
            "type": "block_actions",
            "team": { "id": "T123" },
            "user": { "id": "U123", "name": "ada" },
            "actions": [{
                "action_id": "interview.answers.yes",
                "type": "button",
                "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
            }]
        });
        assert!(parse_interaction(&payload).is_none());
    }

    /// The dotted prefix constant must stay in sync with the canonical
    /// prefix so outbound and inbound never drift.
    #[test]
    fn dotted_prefix_constant_matches_canonical_prefix() {
        assert_eq!(
            ANSWER_ACTION_ID_PREFIX_DOT,
            format!("{ANSWER_ACTION_ID_PREFIX}.")
        );
    }
}
