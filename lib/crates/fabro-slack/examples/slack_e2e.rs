#![allow(clippy::print_stderr, clippy::absolute_paths, clippy::exit)]

use std::sync::Arc;

use fabro_interview::{
    Answer, AnswerValue, Interviewer, Question, QuestionOption, QuestionType, WebInterviewer,
};
use fabro_slack::blocks::{answered_blocks, question_to_blocks};
use fabro_slack::client::{PostedMessage, SlackClient};
use fabro_slack::connection;
use fabro_slack::threads::ThreadRegistry;

struct TestCase {
    label: &'static str,
    question: Question,
}

fn test_cases() -> Vec<TestCase> {
    let mut mc = Question::new("Pick a language:", QuestionType::MultipleChoice);
    mc.options = vec![
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

    let mut ms = Question::new("Select features to enable:", QuestionType::MultiSelect);
    ms.options = vec![
        QuestionOption {
            key: "auth".to_string(),
            label: "Authentication".to_string(),
        },
        QuestionOption {
            key: "billing".to_string(),
            label: "Billing".to_string(),
        },
        QuestionOption {
            key: "notifications".to_string(),
            label: "Notifications".to_string(),
        },
    ];

    vec![
        TestCase {
            label: "YesNo",
            question: Question::new("Do you approve this deployment?", QuestionType::YesNo),
        },
        TestCase {
            label: "Confirmation",
            question: Question::new(
                "This will delete all staging data. Continue?",
                QuestionType::Confirmation,
            ),
        },
        TestCase {
            label: "MultipleChoice",
            question: mc,
        },
        TestCase {
            label: "MultiSelect",
            question: ms,
        },
        TestCase {
            label: "Freeform",
            question: Question::new("What is the repository URL?", QuestionType::Freeform),
        },
    ]
}

fn format_answer(answer: &Answer) -> String {
    match &answer.value {
        AnswerValue::Yes => "Yes".to_string(),
        AnswerValue::No => "No".to_string(),
        AnswerValue::Aborted => "Aborted".to_string(),
        AnswerValue::Text(t) => t.clone(),
        AnswerValue::Selected(k) => {
            if let Some(opt) = &answer.selected_option {
                format!("{} ({})", opt.label, k)
            } else {
                k.clone()
            }
        }
        AnswerValue::MultiSelected(keys) => keys.join(", "),
        AnswerValue::Skipped => "Skipped".to_string(),
        AnswerValue::Timeout => "Timed out".to_string(),
    }
}

async fn ask_question(
    test_case: TestCase,
    interviewer: &Arc<WebInterviewer>,
    thread_registry: &ThreadRegistry,
    slack_client: &SlackClient,
    channel: &str,
) {
    eprintln!("\n--- {} ---", test_case.label);

    let question_text = test_case.question.text.clone();
    let is_freeform = test_case.question.question_type == QuestionType::Freeform;
    let interviewer_clone = Arc::clone(interviewer);
    let ask_handle = tokio::spawn(async move { interviewer_clone.ask(test_case.question).await });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let pending = interviewer.pending_questions();
    let pq = pending
        .iter()
        .find(|pq| pq.question.text == question_text)
        .expect("Question should be pending");

    let question_id = pq.id.clone();
    let blocks = question_to_blocks(&question_id, &pq.question);

    let posted: PostedMessage = slack_client
        .post_message(channel, &blocks, None)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to post message: {e}");
            std::process::exit(1);
        });

    // For freeform questions, register the message ts so thread replies get routed
    if is_freeform {
        thread_registry.register(&posted.ts, &question_id);
        eprintln!("Posted. Reply in thread in Slack...");
    } else {
        eprintln!("Posted. Respond in Slack...");
    }

    let answer = ask_handle.await.expect("ask task panicked");
    let answer_text = format_answer(&answer);
    eprintln!("Got answer: {answer_text}");

    // Clean up thread registration
    if is_freeform {
        thread_registry.remove(&posted.ts);
    }

    let updated = answered_blocks(&question_text, &answer_text);
    if let Err(e) = slack_client
        .update_message(&posted.channel_id, &posted.ts, &updated)
        .await
    {
        eprintln!("Failed to update message: {e}");
    }
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter("fabro_slack=debug,info")
        .init();

    let bot_token = std::env::var("FABRO_SLACK_BOT_TOKEN").expect("FABRO_SLACK_BOT_TOKEN required");
    let app_token = std::env::var("FABRO_SLACK_APP_TOKEN").expect("FABRO_SLACK_APP_TOKEN required");
    let channel = std::env::var("FABRO_SLACK_CHANNEL").unwrap_or_else(|_| "#arc-test".to_string());

    eprintln!("Connecting to Slack Socket Mode...");

    let slack_client = SlackClient::new(bot_token);
    let wss_url = connection::open_socket_url(slack_client.http(), &app_token)
        .await
        .expect("Failed to open socket URL");

    let interviewer = Arc::new(WebInterviewer::new());
    let thread_registry = Arc::new(ThreadRegistry::new());

    // Start the event loop in the background
    let interviewer_for_loop = Arc::clone(&interviewer);
    let thread_registry_for_loop = Arc::clone(&thread_registry);
    tokio::spawn(async move {
        connection::run_event_loop(&wss_url, &interviewer_for_loop, &thread_registry_for_loop)
            .await
            .ok();
    });

    // Wait for the socket to connect
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    eprintln!("Connected. Running all question types...\n");

    let cases = test_cases();
    for case in cases {
        ask_question(
            case,
            &interviewer,
            &thread_registry,
            &slack_client,
            &channel,
        )
        .await;
    }

    eprintln!("\nAll question types tested!");
}
