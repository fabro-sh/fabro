use std::fmt::Write as _;

use fabro_interview::Question;
use fabro_types::QuestionType;
use serde_json::{Value, json};

use crate::payload::{SlackActionPayload, encode_action_value};

pub(crate) const ANSWER_ACTION_ID_PREFIX: &str = "interview.answer";
const MULTI_SELECT_BLOCK_ID: &str = "interview.checkboxes";
const MULTI_SELECT_ACTION_ID: &str = "interview.select";
const MULTI_SELECT_SUBMIT_ACTION_ID: &str = "interview.submit";

/// Slack section block `text.text` is documented to accept at most 3000
/// characters (Unicode scalars). Both the header section and the context
/// preview are capped against this so a pathological question, stage, URL,
/// or LLM-produced context_display can never produce an `invalid_blocks`
/// response. See https://docs.slack.dev/reference/block-kit/blocks/section-block/.
const SLACK_SECTION_TEXT_LIMIT: usize = 3000;

/// Suffix appended when `context_display` is truncated. Included in the
/// budget arithmetic so the final block is guaranteed to fit under the
/// section limit no matter how long the upstream stage's response was.
const CONTEXT_TRUNCATION_SUFFIX: &str =
    "\n…\n_(truncated; open the run in Fabro for the full context)_";

/// Suffix appended when the header text itself exceeds the section limit
/// (e.g. an extremely long question label combined with a long stage name).
const HEADER_TRUNCATION_SUFFIX: &str = " …";

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

fn divider() -> Value {
    json!({ "type": "divider" })
}

/// Escape Slack control characters in untrusted text. Slack treats `<…>`
/// as link/mention syntax and `&` as the escape character, so leaving them
/// raw lets an upstream LLM stage post `<!here>`, `<@U…>`, or `<#C…>`
/// payloads that ping people or surface channels. Escaping these does NOT
/// break legitimate markdown like `*bold*`, `_italic_`, `~strike~`, or
/// `` `code` `` — those characters are not escaped here on purpose so
/// formatted text (e.g. a plan summary) still renders.
/// Per https://docs.slack.dev/messaging/formatting-message-text/#escaping.
fn escape_slack_controls(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Truncate a string to at most `limit` Unicode scalars, appending `suffix`
/// when truncation occurs. `suffix` is included in the budget so the result
/// is always `<= limit` characters total.
fn truncate_to_limit(text: &str, limit: usize, suffix: &str) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let suffix_len = suffix.chars().count();
    let keep = limit.saturating_sub(suffix_len);
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(suffix);
    out
}

/// Build the leading section block for an interview message: question label,
/// stage hint, and a deep link back to the run when one is available. The
/// final text is bounded by Slack's section-text limit so even pathological
/// inputs cannot produce `invalid_blocks`.
fn header_section(question: &Question, run_web_url: Option<&str>) -> Value {
    let mut text = format!("*{}*", escape_slack_controls(&question.text));
    if !question.stage.is_empty() {
        let _ = write!(
            text,
            "  ·  stage `{}`",
            escape_slack_controls(&question.stage)
        );
    }
    if let Some(url) = run_web_url {
        // The URL is server-owned (built from `server.web.url` + run id) and
        // does not flow through escape_slack_controls so the `<…|…>` link
        // syntax is preserved.
        let _ = write!(text, "\n<{url}|Open in Fabro>");
    }
    text_block(&truncate_to_limit(
        &text,
        SLACK_SECTION_TEXT_LIMIT,
        HEADER_TRUNCATION_SUFFIX,
    ))
}

/// Build a context section showing the upstream stage's response so a Slack
/// reviewer has enough information to act on the buttons without having to
/// open the run in the web UI. Slack control characters are escaped (so
/// LLM-produced content can't trigger unintended pings or channel mentions)
/// while leaving Markdown formatting intact. Truncated to fit Slack's
/// section text limit.
fn context_section(context_display: &str) -> Option<Value> {
    let trimmed = context_display.trim();
    if trimmed.is_empty() {
        return None;
    }
    let neutralized = escape_slack_controls(trimmed);
    let bounded = truncate_to_limit(
        &neutralized,
        SLACK_SECTION_TEXT_LIMIT,
        CONTEXT_TRUNCATION_SUFFIX,
    );
    Some(text_block(&bounded))
}

/// Assemble the leading blocks shared by every question shape: header
/// section + optional context preview + a divider before the buttons.
fn lead_blocks(question: &Question, run_web_url: Option<&str>) -> Vec<Value> {
    let mut blocks = vec![header_section(question, run_web_url)];
    if let Some(context_display) = question.context_display.as_deref() {
        if let Some(section) = context_section(context_display) {
            blocks.push(section);
            blocks.push(divider());
        }
    }
    blocks
}

pub fn answered_blocks(question_text: &str, answer_text: &str) -> Vec<Value> {
    vec![text_block(&format!(
        "~{}~\n*Answer:* {}",
        escape_slack_controls(question_text),
        escape_slack_controls(answer_text),
    ))]
}

pub fn question_to_blocks(
    run_id: &str,
    question_id: &str,
    question: &Question,
    run_web_url: Option<&str>,
) -> Vec<Value> {
    let mut blocks = lead_blocks(question, run_web_url);

    match question.question_type {
        QuestionType::YesNo | QuestionType::Confirmation => {
            blocks.push(json!({
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
            }));
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
            blocks.push(json!({
                "type": "actions",
                "elements": elements,
            }));
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
            blocks.push(json!({
                "type": "actions",
                "block_id": MULTI_SELECT_BLOCK_ID,
                "elements": [{
                    "type": "checkboxes",
                    "action_id": MULTI_SELECT_ACTION_ID,
                    "options": options
                }]
            }));
            blocks.push(json!({
                "type": "actions",
                "elements": [
                    button("Submit", &encode_action_value(&SlackActionPayload::SubmitMulti {
                        run_id: run_id.to_string(),
                        qid: question_id.to_string(),
                    }), MULTI_SELECT_SUBMIT_ACTION_ID),
                ]
            }));
        }
        QuestionType::Freeform => {
            blocks.push(text_block(
                "_Reply in thread (mention me with your answer)._",
            ));
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use fabro_types::InterviewOption;

    use super::*;

    #[test]
    fn yes_no_produces_two_buttons() {
        let q = Question::new("Approve this PR?", QuestionType::YesNo);
        let blocks = question_to_blocks("run-1", "q-1", &q, None);
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
        let blocks = question_to_blocks("run-1", "q-2", &q, None);
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
        let blocks = question_to_blocks("run-1", "q-3", &q, None);
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
        let blocks = question_to_blocks("run-1", "q-4", &q, None);
        let blocks_json: Value = serde_json::to_value(&blocks).unwrap();

        let arr = blocks_json.as_array().unwrap();
        assert_eq!(arr.len(), 2, "header section + thread-reply prompt");
        let header_text = arr[0]["text"]["text"].as_str().unwrap();
        assert!(header_text.contains("What's the repo URL?"));
        let prompt_text = arr[1]["text"]["text"].as_str().unwrap();
        assert!(prompt_text.contains("Reply in thread"));
        assert!(prompt_text.contains("mention me"));
    }

    #[test]
    fn action_values_include_run_id_and_question_id() {
        let q = Question::new("Approve?", QuestionType::YesNo);
        let blocks = question_to_blocks("run-7", "q-7", &q, None);
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
    fn header_includes_run_link_when_url_provided() {
        let q = Question::new("Approve Plan", QuestionType::YesNo);
        let blocks = question_to_blocks(
            "run-1",
            "q-1",
            &q,
            Some("http://127.0.0.1:32276/runs/run-1"),
        );
        let header = serde_json::to_value(&blocks).unwrap()[0]["text"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(header.contains("<http://127.0.0.1:32276/runs/run-1|Open in Fabro>"));
    }

    #[test]
    fn header_omits_link_when_url_missing() {
        let q = Question::new("Approve Plan", QuestionType::YesNo);
        let blocks = question_to_blocks("run-1", "q-1", &q, None);
        let header = serde_json::to_value(&blocks).unwrap()[0]["text"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!header.contains("Open in Fabro"));
    }

    #[test]
    fn header_shows_stage_when_present() {
        let mut q = Question::new("Approve Plan", QuestionType::YesNo);
        q.stage = "plan".to_string();
        let blocks = question_to_blocks("run-1", "q-1", &q, None);
        let header = serde_json::to_value(&blocks).unwrap()[0]["text"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(header.contains("stage `plan`"));
    }

    #[test]
    fn header_truncates_when_inputs_exceed_section_limit() {
        let mut q = Question::new("a".repeat(4000), QuestionType::YesNo);
        q.stage = "b".repeat(2000);
        let blocks = question_to_blocks(
            "run-1",
            "q-1",
            &q,
            Some("http://127.0.0.1:32276/runs/run-1"),
        );
        let header = serde_json::to_value(&blocks).unwrap()[0]["text"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            header.chars().count() <= 3000,
            "header text exceeded Slack section limit: {} chars",
            header.chars().count()
        );
        assert!(header.ends_with(" …"));
    }

    #[test]
    fn context_display_renders_between_header_and_actions() {
        let mut q = Question::new("Approve Plan", QuestionType::YesNo);
        q.context_display = Some(
            "Plan artifact created and published.\n\n\
             - Local artifact: tmp-docs/fabro-plan.html\n\
             - Dossier canonical URL: https://example.test/s/siv-1067/eng-design-doc"
                .to_string(),
        );
        let blocks_json =
            serde_json::to_value(question_to_blocks("run-1", "q-1", &q, None)).unwrap();
        let arr = blocks_json.as_array().unwrap();
        // 0: header section, 1: context section, 2: divider, 3: actions
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["type"], "section");
        assert_eq!(arr[1]["type"], "section");
        assert_eq!(arr[2]["type"], "divider");
        assert_eq!(arr[3]["type"], "actions");
        let context_text = arr[1]["text"]["text"].as_str().unwrap();
        assert!(context_text.contains("Plan artifact created"));
        assert!(context_text.contains("tmp-docs/fabro-plan.html"));
    }

    #[test]
    fn context_display_truncates_oversized_text_to_fit_slack_budget() {
        let mut q = Question::new("Approve Plan", QuestionType::YesNo);
        q.context_display = Some("x".repeat(10_000));
        let blocks_json =
            serde_json::to_value(question_to_blocks("run-1", "q-1", &q, None)).unwrap();
        let context_text = blocks_json[1]["text"]["text"].as_str().unwrap();
        assert!(
            context_text.chars().count() <= 3000,
            "context block exceeded Slack section text limit: {} chars",
            context_text.chars().count()
        );
        assert!(context_text.contains("truncated"));
    }

    #[test]
    fn empty_context_display_is_skipped() {
        let mut q = Question::new("Approve Plan", QuestionType::YesNo);
        q.context_display = Some("   \n\t  ".to_string());
        let blocks_json =
            serde_json::to_value(question_to_blocks("run-1", "q-1", &q, None)).unwrap();
        let arr = blocks_json.as_array().unwrap();
        // Falls back to header + actions when there's nothing meaningful.
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "section");
        assert_eq!(arr[1]["type"], "actions");
    }

    #[test]
    fn slack_control_chars_in_question_text_are_escaped() {
        let q = Question::new("Approve <plan> & merge?", QuestionType::YesNo);
        let blocks_json =
            serde_json::to_value(question_to_blocks("run-1", "q-1", &q, None)).unwrap();
        let header = blocks_json[0]["text"]["text"].as_str().unwrap();
        // &, <, > must be escaped so Slack doesn't reinterpret them as link
        // or mention syntax. Other Markdown metacharacters (*, _, ~, `) are
        // intentionally left untouched so legitimate formatting still renders.
        assert!(header.contains("&lt;plan&gt;"));
        assert!(header.contains("&amp;"));
    }

    #[test]
    fn slack_control_chars_in_context_display_are_escaped() {
        // An LLM-produced context_display could embed `<!here>`, `<@U…>`, or
        // `<#C…>` which Slack would treat as a notification or mention. The
        // escape must neutralise them while keeping bullets/bold/code intact.
        let mut q = Question::new("Approve Plan", QuestionType::YesNo);
        q.context_display = Some(
            "Heads up: <!here> please review\n\
             - tagged: <@U12345>\n\
             - moved channel: <#C67890>\n\
             - kept: *bold* _italic_ `code` ~strike~"
                .to_string(),
        );
        let blocks_json =
            serde_json::to_value(question_to_blocks("run-1", "q-1", &q, None)).unwrap();
        let context = blocks_json[1]["text"]["text"].as_str().unwrap();
        // Pings are neutralised.
        assert!(!context.contains("<!here>"));
        assert!(!context.contains("<@U12345>"));
        assert!(!context.contains("<#C67890>"));
        assert!(context.contains("&lt;!here&gt;"));
        assert!(context.contains("&lt;@U12345&gt;"));
        assert!(context.contains("&lt;#C67890&gt;"));
        // Markdown formatting is preserved.
        assert!(context.contains("*bold*"));
        assert!(context.contains("_italic_"));
        assert!(context.contains("`code`"));
        assert!(context.contains("~strike~"));
    }

    #[test]
    fn answered_blocks_escape_slack_control_chars() {
        let blocks = answered_blocks("Approve <plan>?", "Yes & ship");
        let text = serde_json::to_value(&blocks).unwrap()[0]["text"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!text.contains("<plan>"));
        assert!(text.contains("&lt;plan&gt;"));
        assert!(text.contains("Yes &amp; ship"));
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
        let blocks = question_to_blocks("run-1", "q-5", &q, None);
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
