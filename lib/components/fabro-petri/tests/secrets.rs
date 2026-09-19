//! A `{{ secrets.NAME }}` reference resolves from the vault into a
//! command's environment, and the value never reaches `petri_records`:
//! Petri masks every record before it is appended.
//!
//! The run takes its host scope through the sandbox-driver host plugin, so
//! the test skips, and says why, when the executable is not found, unless
//! `FABRO_REQUIRE_SANDBOX_PLUGINS` is set.

mod support;

use std::collections::HashMap;
use std::sync::Arc;

use fabro_petri::SqliteRunStore;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::secrets::VaultSecrets;
use fabro_store::test_support;
use fabro_types::SecretType;
use fabro_vault::Vault;
use support::{Silent, admit, host_plugin, no_questions, run_request};

const TOKEN: &str = "hunter2-hunter2-hunter2";

/// A command that checks the secret reached its environment and then
/// prints it, so the value would land in a log line if nothing masked it.
const WORKFLOW: &str = r#"digraph Secret {
    graph [goal="Use a secret"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="test \"$TOKEN\" = hunter2-hunter2-hunter2 && echo \"token is $TOKEN\""]
    start -> say -> exit
}"#;

const SETTINGS: &str = r#"_version = 1

[workflow]
graph = "workflow.fabro"

[run.environment]
id = "local"

[environments.local]
provider = "local"

[environments.local.env]
TOKEN = "{{ secrets.TOKEN }}"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_reaches_the_command_and_is_masked_in_every_record() {
    if host_plugin().is_none() {
        return;
    }
    let root = tempfile::tempdir().expect("a temp dir");
    let pool = test_support::in_memory_pool_with(&[
        fabro_db::BLOBS_MIGRATION_SQL,
        fabro_db::PETRI_RECORDS_MIGRATION_SQL,
    ]);
    let store = Arc::new(SqliteRunStore::new(pool.clone()));
    let mut vault = Vault::from_entries(HashMap::new());
    vault
        .set("TOKEN", TOKEN, SecretType::Token, None)
        .expect("a detached vault takes an entry");
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", WORKFLOW), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let mut request = run_request(
        "secret",
        &root.path().join("run"),
        graphs,
        store.clone(),
        runtime,
        no_questions(Arc::new(Silent)),
    );
    request.secrets = Some(Arc::new(VaultSecrets::from_vault(&vault)));

    let outcome = engine::run(request).await.expect("the run ends");

    assert_eq!(
        outcome.status,
        RunStatus::Success,
        "the command saw the secret: {outcome:?}"
    );
    let records: Vec<String> = sqlx::query_scalar("SELECT record_json FROM petri_records")
        .fetch_all(&pool)
        .await
        .expect("the records read");
    assert!(!records.is_empty());
    assert!(
        records.iter().all(|record| !record.contains(TOKEN)),
        "the secret's value is in a record"
    );
    assert!(
        records.iter().any(|record| record.contains("token is ***")),
        "the command's output was masked, not dropped"
    );
}

/// Without a provider the reference resolves to nothing and the command
/// fails on the missing secret, as the standalone runner's does; the run
/// ends the way Fabro's failure policy for a command ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_nobody_provides_fails_the_command() {
    if host_plugin().is_none() {
        return;
    }
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(petri_store::MemoryRunStore::new());
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", WORKFLOW), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "unprovided",
        &root.path().join("run"),
        graphs,
        store,
        runtime,
        no_questions(Arc::new(Silent)),
    );

    let outcome = engine::run(request).await.expect("the run ends");

    assert!(
        outcome
            .failure
            .as_deref()
            .is_some_and(|failure| failure.contains("no secret named `TOKEN`")),
        "{outcome:?}"
    );
}
