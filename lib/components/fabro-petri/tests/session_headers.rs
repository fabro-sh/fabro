//! Recording HTTP providers exercise the actual run client and native steps.
mod support;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use fabro_auth::VaultCredentialSource;
use fabro_llm::test_support;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::runtime::{self, RuntimeSpec};
use fabro_types::{RunId, SecretType};
use fabro_vault::Vault;
use httpmock::{Method, MockServer};
use lithos_llm::catalog::{Catalog, ProviderId};
use lithos_llm::types::Request;
use petri_store::MemoryRunStore;
use serde_json::{Value, json};
use tokio::sync::RwLock;

fn credentials() -> Arc<VaultCredentialSource> {
    let mut vault = Vault::from_entries(HashMap::new());
    for name in [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "CUSTOM_API_KEY",
        "SESSION",
    ] {
        vault
            .set(name, "fixture-value", SecretType::Token, None)
            .expect("the detached vault takes fixture secrets");
    }
    Arc::new(VaultCredentialSource::vault_only(Arc::new(RwLock::new(
        vault,
    ))))
}

fn catalog(server: &MockServer, provider: &str, header: Option<&str>) -> Catalog {
    let custom = if provider == "custom" {
        "display_name = 'Custom'\ncodecs = ['openai-chat']\nauth = { type = 'bearer' }\nallow_passthrough = true\nmetadata = { agent = { profile = 'openai' } }\n"
    } else {
        ""
    };
    let header = header
        .map(|value| format!("default_headers = {{ 'X-SeSsIoN-Id' = '{value}' }}\n"))
        .unwrap_or_default();
    test_support::test_catalog_with_overlay(&format!(
        "[providers.{provider}]\nbase_url = '{}'\n{custom}{header}",
        server.url("/v1")
    ))
}

fn model(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-5.4",
        "anthropic" => "claude-sonnet-4-6",
        "openrouter" => "openai/gpt-5.4",
        _ => "fixture-model",
    }
}

fn path(provider: &str) -> &'static str {
    match provider {
        "openai" => "/v1/responses",
        "anthropic" => "/v1/messages",
        _ => "/v1/chat/completions",
    }
}

fn response(provider: &str) -> Value {
    match provider {
        "openai" => {
            json!({"id":"resp_1","object":"response","status":"completed","model":model(provider),
            "output":[{"type":"message","id":"msg_1","role":"assistant","status":"completed",
                "content":[{"type":"output_text","text":"Done","annotations":[]}]}],
            "usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}})
        }
        "anthropic" => {
            json!({"id":"msg_1","type":"message","role":"assistant","model":model(provider),
            "content":[{"type":"text","text":"Done"}],"stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":10,"output_tokens":2}})
        }
        _ => json!({"id":"chat_1","object":"chat.completion","created":1,"model":model(provider),
            "choices":[{"index":0,"message":{"role":"assistant","content":"Done"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
    }
}

fn stream(provider: &str) -> String {
    match provider {
        "openai" => format!("event: response.completed\ndata: {}\n\n", json!({"type":"response.completed","response":response(provider)})),
        "anthropic" => [
            json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":model(provider),"content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Done"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ].iter().fold(String::new(), |mut output, event| {
            write!(output, "event: {}\ndata: {event}\n\n", event["type"].as_str().expect("fixture event type")).expect("writing to a string");
            output
        }),
        _ => format!("data: {}\n\ndata: [DONE]\n\n", json!({"id":"chat_1","object":"chat.completion.chunk","created":1,"model":model(provider),"choices":[{"index":0,"delta":{"role":"assistant","content":"Done"},"finish_reason":"stop"}]})),
    }
}

fn client(
    server: &MockServer,
    provider: &str,
    run_id: Option<RunId>,
    header: Option<&str>,
) -> lithos_llm::Client {
    runtime::model_client(
        catalog(server, provider, header),
        credentials(),
        Some(fabro_test::test_http_client()),
        &[ProviderId::new(provider)],
        run_id,
    )
    .expect("the model client builds")
    .expect("the provider is eligible")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_agents_and_prompts_send_the_run_id_across_http_providers() {
    for provider in ["openai", "openrouter", "anthropic", "custom"] {
        let server = MockServer::start_async().await;
        let run_id = RunId::new();
        let session = run_id.to_string();
        let prompt = server.mock(|when, then| {
            when.method(Method::POST)
                .path(path(provider))
                .header("x-session-id", &session)
                .header(
                    if provider == "anthropic" {
                        "x-api-key"
                    } else {
                        "authorization"
                    },
                    if provider == "anthropic" {
                        "fixture-value"
                    } else {
                        "Bearer fixture-value"
                    },
                )
                .json_body_excludes(json!({"stream":true}).to_string());
            then.status(200).json_body(response(provider));
        });
        let agent = server.mock(|when, then| {
            when.method(Method::POST)
                .path(path(provider))
                .header("x-session-id", &session)
                .header(
                    if provider == "anthropic" {
                        "x-api-key"
                    } else {
                        "authorization"
                    },
                    if provider == "anthropic" {
                        "fixture-value"
                    } else {
                        "Bearer fixture-value"
                    },
                )
                .json_body_includes(json!({"stream":true}).to_string());
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(stream(provider));
        });
        let runtime = RuntimeSpec {
            model_client: Some(client(&server, provider, Some(run_id), None)),
            ..RuntimeSpec::default()
        };
        let workflow = format!(
            r#"digraph Headers {{
            graph [backend="api", default_max_retries=0]
            start [shape=Mdiamond]
            agent [shape=box, prompt="Say Done", model="{}", provider="{provider}"]
            prompt [shape=tab, prompt="Say Done", model="{}", provider="{provider}"]
            exit [shape=Msquare]
            start -> agent -> prompt -> exit
        }}"#,
            model(provider),
            model(provider)
        );
        let graphs = support::admit(
            &[
                ("workflow.fabro", &workflow),
                ("workflow.toml", support::SETTINGS),
            ],
            Launch::default(),
            &runtime,
        );
        let root = tempfile::tempdir().unwrap();
        let request = support::run_request(
            &session,
            root.path(),
            graphs,
            Arc::new(MemoryRunStore::new()),
            runtime,
            support::no_questions(Arc::new(support::Silent)),
        );
        let outcome = engine::run(request).await.unwrap();
        assert_eq!(
            outcome.status,
            RunStatus::Success,
            "{provider}: {outcome:?}"
        );
        prompt.assert_calls(1);
        agent.assert_calls(1);
    }
}

#[tokio::test]
async fn explicit_headers_win_and_unscoped_clients_do_not_inherit_a_run() {
    for provider in ["openai", "openrouter", "anthropic", "custom"] {
        for configured in [
            None,
            Some("operator-session"),
            Some("{{ secrets.SESSION }}"),
        ] {
            let server = MockServer::start_async().await;
            let run_id = RunId::new();
            let session = run_id.to_string();
            let expected = match configured {
                None => session.as_str(),
                Some("operator-session") => "operator-session",
                _ => "fixture-value",
            };
            let scoped = server.mock(|when, then| {
                when.method(Method::POST)
                    .path(path(provider))
                    .header("x-session-id", expected);
                then.status(200).json_body(response(provider));
            });
            let request = || {
                Request::builder()
                    .model(format!("{provider}/{}", model(provider)))
                    .user("Say Done")
                    .build()
                    .unwrap()
            };
            client(&server, provider, Some(run_id), configured)
                .complete(request())
                .await
                .unwrap();
            scoped.assert_calls(1);
        }
        let server = MockServer::start_async().await;
        let unscoped = server.mock(|when, then| {
            when.method(Method::POST)
                .path(path(provider))
                .header_missing("x-session-id");
            then.status(200).json_body(response(provider));
        });
        client(&server, provider, None, None)
            .complete(
                Request::builder()
                    .model(format!("{provider}/{}", model(provider)))
                    .user("Say Done")
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();
        unscoped.assert_calls(1);
    }
}

#[tokio::test]
async fn concurrent_runs_and_rebuilt_clients_keep_their_own_identity() {
    let server = MockServer::start_async().await;
    let first = RunId::new();
    let second = RunId::new();
    let one = server.mock(|when, then| {
        when.header("x-session-id", first.to_string());
        then.status(200).json_body(response("openai"));
    });
    let two = server.mock(|when, then| {
        when.header("x-session-id", second.to_string());
        then.status(200).json_body(response("openai"));
    });
    let request = || {
        Request::builder()
            .model("openai/gpt-5.4")
            .user("Say Done")
            .build()
            .unwrap()
    };
    let a = client(&server, "openai", Some(first), None);
    let b = client(&server, "openai", Some(second), None);
    let (a, b) = tokio::join!(a.complete(request()), b.complete(request()));
    a.unwrap();
    b.unwrap();
    // Workers reconstruct their clients from the persisted run ID on resume.
    client(&server, "openai", Some(first), None)
        .complete(request())
        .await
        .unwrap();
    one.assert_calls(2);
    two.assert_calls(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_fallback_keeps_the_run_id_on_both_providers() {
    let server = MockServer::start_async().await;
    let run_id = RunId::new();
    let session = run_id.to_string();
    let primary = server.mock(|when, then| {
        when.method(Method::POST)
            .path(path("openai"))
            .header("x-session-id", &session);
        then.status(401).json_body(
            json!({"error":{"type":"authentication_error","message":"fixture rejection"}}),
        );
    });
    let fallback = server.mock(|when, then| {
        when.method(Method::POST)
            .path(path("anthropic"))
            .header("x-session-id", &session);
        then.status(200).json_body(response("anthropic"));
    });
    let catalog = test_support::test_catalog_with_overlay(&format!(
        "[providers.openai]\nbase_url = '{}'\n[providers.anthropic]\nbase_url = '{}'\n",
        server.url("/v1"),
        server.url("/v1")
    ));
    let client = runtime::model_client(
        catalog,
        credentials(),
        Some(fabro_test::test_http_client()),
        &[ProviderId::new("openai"), ProviderId::new("anthropic")],
        Some(run_id),
    )
    .unwrap()
    .unwrap();
    let runtime = RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    };
    let workflow = r#"digraph Fallback {
        graph [backend="api", default_max_retries=0]
        start [shape=Mdiamond]
        prompt [shape=tab, prompt="Say Done", model="gpt-5.4", provider="openai"]
        exit [shape=Msquare]
        start -> prompt -> exit
    }"#;
    let settings = format!(
        "{}\n[run.model.fallbacks]\n'gpt-5.4' = ['anthropic:claude-sonnet-4-6']\n",
        support::SETTINGS
    );
    let graphs = support::admit(
        &[("workflow.fabro", workflow), ("workflow.toml", &settings)],
        Launch::default(),
        &runtime,
    );
    let root = tempfile::tempdir().unwrap();
    let request = support::run_request(
        &session,
        root.path(),
        graphs,
        Arc::new(MemoryRunStore::new()),
        runtime,
        support::no_questions(Arc::new(support::Silent)),
    );
    let outcome = engine::run(request).await.unwrap();
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    primary.assert_calls(1);
    fallback.assert_calls(1);
}

#[tokio::test]
async fn retried_http_calls_keep_the_run_id() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fabro_auth::ExtraHeadersCredentialSource;
    use fabro_llm::ClientOptions;
    use httpmock::HttpMockResponse;

    let server = MockServer::start_async().await;
    let run_id = RunId::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let mock = server.mock(|when, then| {
        when.method(Method::POST)
            .path(path("openai"))
            .header("x-session-id", run_id.to_string());
        then.respond_with(move |_| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                HttpMockResponse::builder()
                    .status(503)
                    .body(r#"{"error":{"message":"try again","type":"server_error"}}"#)
                    .build()
            } else {
                HttpMockResponse::builder()
                    .status(200)
                    .body(response("openai").to_string())
                    .build()
            }
        });
    });
    let credentials = Arc::new(ExtraHeadersCredentialSource::for_run(credentials(), run_id));
    let built = fabro_llm::build_client(
        catalog(&server, "openai", None),
        credentials,
        ClientOptions {
            http: Some(fabro_test::test_http_client()),
            retry: Some(test_support::test_retry_policy()),
            ..ClientOptions::default()
        },
    )
    .await
    .unwrap();
    let reply = built
        .client
        .complete(
            Request::builder()
                .model("openai/gpt-5.4")
                .user("Say Done")
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.text(), "Done");
    mock.assert_calls(2);
}
