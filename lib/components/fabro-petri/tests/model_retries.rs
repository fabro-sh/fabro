//! Exercise the production runtime client against the local provider twin.
//! A retry must repeat only the failed model request, not a completed tool.

mod support;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fabro_auth::VaultCredentialSource;
use fabro_llm::test_support;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::runtime::{self, RuntimeSpec};
use fabro_test::{TwinScenario, TwinScenarios, TwinToolCall, twin_openai};
use fabro_types::SecretType;
use fabro_util::shell;
use fabro_vault::Vault;
use lithos_llm::Client;
use lithos_llm::catalog::ProviderId;
use lithos_llm::types::{ErrorKind, Request};
use petri_store::MemoryRunStore;
use serde_json::{Value, json};
use tokio::fs;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

const MODEL: &str = "gpt-5.4";

fn client(base_url: &str, namespace: &str) -> Client {
    let mut vault = Vault::from_entries(HashMap::new());
    vault
        .set("OPENAI_API_KEY", namespace, SecretType::Token, None)
        .expect("the test key is stored in its detached vault");
    runtime::model_client(
        test_support::test_catalog_with_provider_base_url("openai", base_url),
        Arc::new(VaultCredentialSource::vault_only(Arc::new(RwLock::new(
            vault,
        )))),
        Some(fabro_test::test_http_client()),
        &[ProviderId::new("openai")],
    )
    .expect("the production model client builds")
    .expect("the test provider is eligible")
}

fn request() -> Request {
    Request::builder()
        .model(format!("openai/{MODEL}"))
        .user("Say done")
        .build()
        .expect("the test model request is valid")
}

#[tokio::test]
async fn transient_server_failure_retries_the_same_completion() {
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(MODEL).error(520, "temporarily unavailable"))
        .scenario(TwinScenario::responses(MODEL).text("done"))
        .load(twin)
        .await;

    client(&twin.base_url, &namespace)
        .complete(request())
        .await
        .expect("the transient request is retried");
    let logs = twin.request_logs(&namespace).await;
    assert_eq!(logs["requests"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn retries_are_bounded_and_permanent_errors_are_not_retried() {
    let twin = twin_openai().await;
    for (status, kind, expected_calls) in [
        (500, ErrorKind::Server, 3),
        (401, ErrorKind::Authentication, 1),
        (400, ErrorKind::InvalidRequest, 1),
    ] {
        let namespace = format!("{}::{}::{status}", module_path!(), line!());
        let mut scenarios = TwinScenarios::new(&namespace);
        for _ in 0..4 {
            scenarios = scenarios.scenario(TwinScenario::responses(MODEL).error(status, "failed"));
        }
        scenarios.load(twin).await;
        let error = client(&twin.base_url, &namespace)
            .complete(request())
            .await
            .expect_err("exhausted or permanent failures remain failures");
        assert_eq!(error.kind(), kind);
        let logs = twin.request_logs(&namespace).await;
        assert_eq!(logs["requests"].as_array().unwrap().len(), expected_calls);
    }
}

fn retry_events(records: &[Value]) -> Vec<&Value> {
    records
        .iter()
        .filter_map(|record| record.pointer("/body/ev/custom/event/event/LlmRetry"))
        .collect()
}

async fn run_agent(client: Client, root: &Path) -> (RunStatus, Vec<Value>) {
    let runtime = RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    };
    let graph = r#"digraph Retries {
        graph [backend="api", default_max_retries=0, on_failure="exit"]
        start [shape=Mdiamond]
        work [shape=box, prompt="Do the requested work", max_retries=0]
        exit [shape=Msquare]
        start -> work -> exit
    }"#;
    let graphs = support::admit(
        &[
            ("workflow.fabro", graph),
            ("workflow.toml", support::SETTINGS),
        ],
        Launch {
            default_model: Some(format!("openai/{MODEL}")),
            ..Launch::default()
        },
        &runtime,
    );
    let store = Arc::new(MemoryRunStore::new());
    let run = support::run_request(
        "model-retries",
        root,
        graphs,
        store.clone(),
        runtime,
        support::no_questions(Arc::new(support::Silent)),
    );
    let result = engine::run(run)
        .await
        .expect("the run reaches a terminal state");
    (
        result.status,
        support::all_records(store.as_ref(), "model-retries").await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_retries_before_and_after_a_tool_without_repeating_its_effect() {
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let root = tempfile::tempdir().unwrap();
    let counter = root.path().join("tool-executions");
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(MODEL).error(520, "before tool"))
        .scenario(TwinScenario::responses(MODEL).tool_call(TwinToolCall::new(
            "shell",
            json!({"command": format!("printf x >> {}", shell::shell_quote(&counter.to_string_lossy()))}),
        )))
        .scenario(TwinScenario::responses(MODEL).error(500, "after tool"))
        .scenario(TwinScenario::responses(MODEL).text("done"))
        .load(twin)
        .await;

    let (status, records) = run_agent(client(&twin.base_url, &namespace), root.path()).await;
    assert_eq!(status, RunStatus::Success);
    assert_eq!(
        fs::read_to_string(counter).await.ok().as_deref(),
        Some("x"),
        "{records:?}"
    );
    assert_eq!(
        twin.request_logs(&namespace).await["requests"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        retry_events(&records).len(),
        2,
        "retries must be visible: {records:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_failure_is_retried_twice_then_reported_as_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::clone(&connections);
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    let root = tempfile::tempdir().unwrap();
    let (status, records) = run_agent(
        client(&format!("http://{address}/v1"), "network-retries"),
        root.path(),
    )
    .await;
    server.abort();
    assert_ne!(status, RunStatus::Success);
    assert_eq!(connections.load(Ordering::SeqCst), 3);
    let retries = retry_events(&records);
    assert_eq!(
        retries.len(),
        2,
        "network failures must use the bounded retry policy: {records:?}"
    );
    assert!(
        retries
            .iter()
            .all(|retry| retry["error"]["llm_kind"] == "network"),
        "{retries:?}"
    );
}
