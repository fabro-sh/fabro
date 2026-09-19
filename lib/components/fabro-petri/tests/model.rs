//! A model call from a Petri run authenticates through Fabro's vault, and
//! the skills step reads the Fabro home the runtime was given.
//!
//! The `hello` bundle's agent stage calls the OpenAI twin through a model
//! client built over a vault that holds the key; the twin requires a
//! bearer token and logs requests under it, so a request logged under the
//! vault's key proves the key came from the vault. The run takes its host
//! scope through the sandbox-driver host plugin, so the test skips, and
//! says why, when the executable is not found, unless
//! `FABRO_REQUIRE_SANDBOX_PLUGINS` is set.

mod support;

use std::collections::HashMap;
use std::sync::Arc;

use fabro_auth::VaultCredentialSource;
use fabro_llm::test_support::test_catalog_with_provider_base_url;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::runtime::{self, RuntimeSpec};
use fabro_test::{TwinScenario, TwinScenarios, twin_openai};
use fabro_types::SecretType;
use fabro_vault::Vault;
use lithos_llm::catalog::ProviderId;
use petri_store::MemoryRunStore;
use support::{Silent, all_records, hello_bundle, host_plugin, no_questions, run_request};
use tokio::fs;
use tokio::sync::RwLock as AsyncRwLock;

const OPENAI_MODEL: &str = "gpt-5.4";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_call_authenticates_through_the_vault_and_skills_read_the_home() {
    if host_plugin().is_none() {
        return;
    }
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    TwinScenarios::new(&namespace)
        .scenario(TwinScenario::responses(OPENAI_MODEL).text("A haiku, added."))
        .load(twin)
        .await;
    let root = tempfile::tempdir().expect("a temp dir");
    let home = root.path().join("fabro-home");
    std::fs::create_dir_all(home.join("skills")).expect("the skills dir creates");

    // The vault holds the key; nothing in the environment does.
    let mut vault = Vault::from_entries(HashMap::new());
    vault
        .set("OPENAI_API_KEY", &namespace, SecretType::Token, None)
        .expect("a detached vault takes an entry");
    let credentials = Arc::new(VaultCredentialSource::vault_only(Arc::new(
        AsyncRwLock::new(vault),
    )));
    let catalog = test_catalog_with_provider_base_url("openai", &twin.base_url);
    let client = runtime::model_client(catalog, credentials, None, &[ProviderId::new("openai")])
        .expect("the model client builds")
        .expect("openai is eligible");
    let runtime = RuntimeSpec {
        model_client: Some(client),
        fabro_home: Some(home.clone()),
        ..RuntimeSpec::default()
    };

    let workflow = fs::read_to_string(hello_bundle().join("workflow.fabro"))
        .await
        .expect("the hello workflow is checked in");
    let settings = fs::read_to_string(hello_bundle().join("workflow.toml"))
        .await
        .expect("the hello settings are checked in");
    let graphs = support::admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", &settings)],
        Launch {
            model: Some(OPENAI_MODEL.to_string()),
            ..Launch::default()
        },
        &runtime,
    );
    let store = Arc::new(MemoryRunStore::new());
    let request = run_request(
        "hello",
        &root.path().join("run"),
        graphs,
        store.clone(),
        runtime,
        no_questions(Arc::new(Silent)),
    );

    let outcome = engine::run(request).await.expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let logs = twin.request_logs(&namespace).await;
    let requests = logs["requests"]
        .as_array()
        .expect("twin request logs are an array");
    assert!(
        requests
            .iter()
            .any(|request| request["model"] == OPENAI_MODEL),
        "the stage should have called the twin with the vault's key, got {logs}"
    );
    let records = all_records(store.as_ref(), "hello").await;
    let resolved = records
        .iter()
        .find(|record| record.to_string().contains("\"attractor.skills\""))
        .unwrap_or_else(|| panic!("the skills step recorded what it searched: {records:?}"));
    let configured = home.join("skills").display().to_string();
    assert!(
        resolved.to_string().contains(&configured),
        "the configured home is searched: {resolved}"
    );
}
