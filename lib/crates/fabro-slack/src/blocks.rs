use fabro_interview::Question;
use fabro_types::QuestionType;
use serde_json::{Value, json};

use crate::payload::{SlackActionPayload, encode_action_value};

pub(crate) const ANSWER_ACTION_ID_PREFIX: &str = "interview.answer";
const MULTI_SELECT_BLOCK_ID: &str = "interview.checkboxes";
const MULTI_SELECT_ACTION_ID: &str = "interview.select";
const MULTI_SELECT_SUBMIT_ACTION_ID: &str = "interview.submit";

/// Build a Slack-unique `action_id` for an interview button.
///
/// Slack requires `action_id`s to be unique within a single message and caps
/// them at 255 characters. The selected option is carried in the button
/// `value` payload, so the `action_id` only needs to be unique — it doesn't
/// have to encode the selection. Suffixes are short, fixed-shape tokens
/// (`yes`, `no`, or the option index) to avoid any character-set or length
/// concerns when option keys are author-supplied.
fn answer_action_id(suffix: &str) -> String {
    format!("{ANSWER_ACTION_ID_PREFIX}.{suffix}")
}

fn text_block(text: &str) -> Value {
    json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": text
        }
    })
}

fn button(label: &str, value: &str, action_id: &str) -> Value {
    json!({
        "type": "button",
        "text": {
            "type": "plain_text",
            "text": label
        },
        "value": value,
        "action_id": action_id
    })
}

pub fn answered_blocks(question_text: &str, answer_text: &str) -> Vec<Value> {
    vec![text_block(&format!(
        "~{question_text}~\n*Answer:* {answer_text}"
    ))]
}

pub fn question_to_blocks(run_id: &str, question_id: &str, question: &Question) -> Vec<Value> {
    let section = text_block(&question.text);

    match question.question_type {
        QuestionType::YesNo | QuestionType::Confirmation => {
            let actions = json!({
                "type": "actions",
                "elements": [
                    button("Yes", &encode_action_value(&SlackActionPayload::Yes {
                        run_id: run_id.to_string(),
                        qid: question_id.to_string(),
                    }), &answer_action_id("yes")),
                    button("No", &encode_action_value(&SlackActionPayload::No {
                        run_id: run_id.to_string(),
                        qid: question_id.to_string(),
                    }), &answer_action_id("no")),
                ]
            });
            vec![section, actions]
        }
        QuestionType::MultipleChoice => {
            let elements: Vec<Value> = question
                .options
                .iter()
                .enumerate()
                .map(|(idx, opt)| {
                    button(
                        &opt.label,
                        &encode_action_value(&SlackActionPayload::Selected {
                            run_id: run_id.to_string(),
                            qid:    question_id.to_string(),
                            key:    opt.key.clone(),
                        }),
                        &answer_action_id(&idx.to_string()),
                    )
                })
                .collect();
            let actions = json!({
                "type": "actions",
                "elements": elements
            });
            vec![section, actions]
        }
        QuestionType::MultiSelect => {
            let options: Vec<Value> = question
                .options
                .iter()
                .map(|opt| {
                    json!({
                        "text": { "type": "plain_text", "text": opt.label },
                        "value": opt.key
                    })
                })
                .collect();
            let checkboxes = json!({
                "type": "actions",
                "block_id": MULTI_SELECT_BLOCK_ID,
                "elements": [{
                    "type": "checkboxes",
                    "action_id": MULTI_SELECT_ACTION_ID,
                    "options": options
                }]
            });
            let submit = json!({
                "type": "actions",
                "elements": [
                    button("Submit", &encode_action_value(&SlackActionPayload::SubmitMulti {
                        run_id: run_id.to_string(),
                        qid: question_id.to_string(),
                    }), MULTI_SELECT_SUBMIT_ACTION_ID),
                ]
            });
            vec![section, checkboxes, submit]
        }
        QuestionType::Freeform => {
            vec![text_block(&format!(
                "{}\n_Please reply in thread (mention me with your answer)._",
                question.text
            ))]
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::InterviewOption;

    use super::*;

    #[test]
    fn yes_no_produces_two_buttons() {
        let q = Question::new("Approve this PR?", QuestionType::YesNo);
        let blocks = question_to_blocks("run-1", "q-1", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let section = &blocks_json[0];
        assert_eq!(section["type"], "section");
        assert!(
            section["text"]["text"]
                .as_str()
                .unwrap()
                .contains("Approve this PR?")
        );

        let actions = &blocks_json[1];
        assert_eq!(actions["type"], "actions");
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["text"]["text"], "Yes");
        assert_eq!(elements[1]["text"]["text"], "No");
    }

    #[test]
    fn confirmation_produces_two_buttons() {
        let q = Question::new("Continue?", QuestionType::Confirmation);
        let blocks = question_to_blocks("run-1", "q-2", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let actions = &blocks_json[1];
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["text"]["text"], "Yes");
        assert_eq!(elements[1]["text"]["text"], "No");
    }

    #[test]
    fn multiple_choice_produces_button_per_option() {
        let mut q = Question::new("Pick a language:", QuestionType::MultipleChoice);
        q.options = vec![
            InterviewOption {
                key:   "rs".to_string(),
                label: "Rust".to_string(),
            },
            InterviewOption {
                key:   "ts".to_string(),
                label: "TypeScript".to_string(),
            },
            InterviewOption {
                key:   "py".to_string(),
                label: "Python".to_string(),
            },
        ];
        let blocks = question_to_blocks("run-1", "q-3", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let actions = &blocks_json[1];
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 3);
        assert_eq!(elements[0]["text"]["text"], "Rust");
        assert_eq!(elements[0]["action_id"], "interview.answer.0");
        assert_eq!(elements[1]["action_id"], "interview.answer.1");
        assert_eq!(elements[2]["action_id"], "interview.answer.2");
        // Slack requires action_id to be unique within a message.
        let ids: std::collections::HashSet<&str> = elements
            .iter()
            .map(|e| e["action_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), elements.len());
        // The option key remains in the button `value` payload so the server
        // can still route the answer regardless of suffix scheme.
        assert!(
            elements[0]["value"]
                .as_str()
                .unwrap()
                .contains("\"key\":\"rs\"")
        );
        assert!(
            elements[0]["value"]
                .as_str()
                .unwrap()
                .contains("\"run_id\":\"run-1\"")
        );
        assert_eq!(elements[1]["text"]["text"], "TypeScript");
        assert_eq!(elements[2]["text"]["text"], "Python");
    }

    #[test]
    fn freeform_produces_section_prompting_thread_reply() {
        let q = Question::new("What's the repo URL?", QuestionType::Freeform);
        let blocks = question_to_blocks("run-1", "q-4", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        assert_eq!(blocks_json.as_array().unwrap().len(), 1);
        let text = blocks_json[0]["text"]["text"].as_str().unwrap();
        assert!(text.contains("What's the repo URL?"));
        assert!(text.contains("reply in thread"));
        assert!(text.contains("mention me"));
    }

    #[test]
    fn action_values_include_run_id_and_question_id() {
        let q = Question::new("Approve?", QuestionType::YesNo);
        let blocks = question_to_blocks("run-7", "q-7", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let actions = &blocks_json[1];
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements[0]["action_id"], "interview.answer.yes");
        assert_eq!(elements[1]["action_id"], "interview.answer.no");
        assert_ne!(elements[0]["action_id"], elements[1]["action_id"]);
        let value = elements[0]["value"].as_str().unwrap();
        assert!(value.contains("\"run_id\":\"run-7\""));
        assert!(value.contains("\"qid\":\"q-7\""));
    }

    #[test]
    fn answered_blocks_show_question_and_answer() {
        let blocks = answered_blocks("Do you approve?", "Yes");
        let json: Value = serde_json::to_value(&blocks).unwrap();

        assert_eq!(json.as_array().unwrap().len(), 1);
        let text = json[0]["text"]["text"].as_str().unwrap();
        assert!(text.contains("Do you approve?"));
        assert!(text.contains("Yes"));
    }

    #[test]
    fn answered_blocks_have_no_actions() {
        let blocks = answered_blocks("Pick one:", "Rust");
        let json: Value = serde_json::to_value(&blocks).unwrap();

        let has_actions = json
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == "actions");
        assert!(!has_actions);
    }

    #[test]
    fn multi_select_produces_checkboxes_and_submit_button() {
        let mut q = Question::new("Select features:", QuestionType::MultiSelect);
        q.options = vec![
            InterviewOption {
                key:   "a".to_string(),
                label: "Auth".to_string(),
            },
            InterviewOption {
                key:   "b".to_string(),
                label: "Billing".to_string(),
            },
        ];
        let blocks = question_to_blocks("run-1", "q-5", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        // Checkboxes in their own block with a block_id
        let checkbox_block = &blocks_json[1];
        assert_eq!(checkbox_block["type"], "actions");
        assert_eq!(checkbox_block["block_id"], MULTI_SELECT_BLOCK_ID);
        let cb_elements = checkbox_block["elements"].as_array().unwrap();
        assert_eq!(cb_elements[0]["type"], "checkboxes");
        assert_eq!(cb_elements[0]["action_id"], MULTI_SELECT_ACTION_ID);

        // Submit button in a separate actions block
        let submit_block = &blocks_json[2];
        assert_eq!(submit_block["type"], "actions");
        let submit_elements = submit_block["elements"].as_array().unwrap();
        assert_eq!(submit_elements[0]["type"], "button");
        assert_eq!(submit_elements[0]["text"]["text"], "Submit");
        assert_eq!(
            submit_elements[0]["action_id"],
            MULTI_SELECT_SUBMIT_ACTION_ID
        );
        assert!(
            submit_elements[0]["value"]
                .as_str()
                .unwrap()
                .contains("\"qid\":\"q-5\"")
        );
    }
}
