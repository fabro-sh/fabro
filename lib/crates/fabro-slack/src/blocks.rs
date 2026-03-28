use fabro_interview::{Question, QuestionType};
use serde_json::{Value, json};

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

pub fn question_to_blocks(question_id: &str, question: &Question) -> Vec<Value> {
    let section = text_block(&question.text);

    match question.question_type {
        QuestionType::YesNo | QuestionType::Confirmation => {
            let actions = json!({
                "type": "actions",
                "elements": [
                    button("Yes", "yes", &format!("{question_id}:yes")),
                    button("No", "no", &format!("{question_id}:no")),
                ]
            });
            vec![section, actions]
        }
        QuestionType::MultipleChoice => {
            let elements: Vec<Value> = question
                .options
                .iter()
                .map(|opt| button(&opt.label, &opt.key, &format!("{question_id}:{}", opt.key)))
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
                "block_id": format!("{question_id}:checkboxes"),
                "elements": [{
                    "type": "checkboxes",
                    "action_id": format!("{question_id}:select"),
                    "options": options
                }]
            });
            let submit = json!({
                "type": "actions",
                "elements": [
                    button("Submit", "submit", &format!("{question_id}:submit")),
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
    use super::*;
    use fabro_interview::QuestionOption;

    #[test]
    fn yes_no_produces_two_buttons() {
        let q = Question::new("Approve this PR?", QuestionType::YesNo);
        let blocks = question_to_blocks("q-1", &q);
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
        let blocks = question_to_blocks("q-2", &q);
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
            QuestionOption {
                key: "rs".to_string(),
                label: "Rust".to_string(),
            },
            QuestionOption {
                key: "ts".to_string(),
                label: "TypeScript".to_string(),
            },
            QuestionOption {
                key: "py".to_string(),
                label: "Python".to_string(),
            },
        ];
        let blocks = question_to_blocks("q-3", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let actions = &blocks_json[1];
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 3);
        assert_eq!(elements[0]["text"]["text"], "Rust");
        assert_eq!(elements[0]["value"], "rs");
        assert_eq!(elements[1]["text"]["text"], "TypeScript");
        assert_eq!(elements[2]["text"]["text"], "Python");
    }

    #[test]
    fn freeform_produces_section_prompting_thread_reply() {
        let q = Question::new("What's the repo URL?", QuestionType::Freeform);
        let blocks = question_to_blocks("q-4", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        assert_eq!(blocks_json.as_array().unwrap().len(), 1);
        let text = blocks_json[0]["text"]["text"].as_str().unwrap();
        assert!(text.contains("What's the repo URL?"));
        assert!(text.contains("reply in thread"));
        assert!(text.contains("mention me"));
    }

    #[test]
    fn question_id_embedded_in_action_ids() {
        let q = Question::new("Approve?", QuestionType::YesNo);
        let blocks = question_to_blocks("q-7", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let actions = &blocks_json[1];
        let elements = actions["elements"].as_array().unwrap();
        assert!(elements[0]["action_id"].as_str().unwrap().contains("q-7"));
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
            QuestionOption {
                key: "a".to_string(),
                label: "Auth".to_string(),
            },
            QuestionOption {
                key: "b".to_string(),
                label: "Billing".to_string(),
            },
        ];
        let blocks = question_to_blocks("q-5", &q);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        // Checkboxes in their own block with a block_id
        let checkbox_block = &blocks_json[1];
        assert_eq!(checkbox_block["type"], "actions");
        assert!(checkbox_block["block_id"].as_str().unwrap().contains("q-5"));
        let cb_elements = checkbox_block["elements"].as_array().unwrap();
        assert_eq!(cb_elements[0]["type"], "checkboxes");

        // Submit button in a separate actions block
        let submit_block = &blocks_json[2];
        assert_eq!(submit_block["type"], "actions");
        let submit_elements = submit_block["elements"].as_array().unwrap();
        assert_eq!(submit_elements[0]["type"], "button");
        assert_eq!(submit_elements[0]["text"]["text"], "Submit");
        assert!(
            submit_elements[0]["action_id"]
                .as_str()
                .unwrap()
                .contains("q-5")
        );
    }
}
