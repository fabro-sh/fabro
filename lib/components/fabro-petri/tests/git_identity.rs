//! Workflow-created commits and checkpoints share the run's Git identity,
//! without configuring Git, on the host and in a clean Docker sandbox.

use std::fmt::Write as _;
use std::sync::Arc;

use fabro_checkpoint::author::GitAuthor;
use fabro_petri::artifacts::StoreArtifactWriter;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::hooks::HooksSpec;
use fabro_petri::providers::SandboxProviderConfig;
use fabro_petri::prune::{self, PruneRequest};
use fabro_petri::runtime::RuntimeSpec;
use fabro_petri::source::{RunSource, SourceRevision};
use fabro_petri::test_support::{MemoryBlobs, MemoryPlatformRecords};
use fabro_store::ArtifactStore;
use fabro_types::settings::run::RunNamespace;
use fabro_types::{RunId, SandboxProviderKind};
use fabro_util::shell;
use object_store::memory::InMemory;
use pebble_coding_agent::test_support::{self, ScriptedCall};
use petri_store::{MemoryRunStore, RunStore};
use serde_json::json;

mod support;

async fn commits_share_identity(
    provider: SandboxProviderKind,
    name: Option<&str>,
    email: Option<&str>,
) {
    let namespace: RunNamespace = serde_json::from_value(json!({
        "git": {"author": {"name": name, "email": email}}
    }))
    .expect("run settings");
    let mut author_settings = String::from("[run.git.author]\n");
    for (key, value) in [("name", name), ("email", email)] {
        if let Some(value) = value {
            // These fixture values have the same escaping in JSON and TOML.
            writeln!(
                author_settings,
                "{key} = {}",
                serde_json::to_string(value).expect("author quoting")
            )
            .expect("author settings");
        }
    }
    let settings = format!(
        r#"{base}
[run.environment]
id = "test"
[environments.test]
provider = "{provider}"
[environments.test.image]
docker = "buildpack-deps:bookworm"
[environments.test.env]
GIT_CONFIG_GLOBAL = "/dev/null"
GIT_CONFIG_NOSYSTEM = "1"
{author_settings}
"#,
        base = support::SETTINGS,
    );
    let author = namespace
        .git
        .author
        .as_ref()
        .map(GitAuthor::from)
        .unwrap_or_default();
    let identity = format!(
        "{}|{}|{}|{}",
        author.name, author.email, author.name, author.email
    );
    let expected = shell::shell_quote(&identity);
    let check_env = format!(
        "test \"$GIT_AUTHOR_NAME|$GIT_AUTHOR_EMAIL|$GIT_COMMITTER_NAME|$GIT_COMMITTER_EMAIL\" = {expected}"
    );
    let commit =
        "git -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -q --allow-empty -m";
    let command = format!(
        "set -eu; {check_env}; ! git config --get user.name; ! git config --get user.email; \
         {commit} command-commit"
    );
    let agent = format!(
        "set -eu; {check_env}; {commit} agent-commit; mkdir extra; git -C extra init -q; \
         cd extra; {commit} extra-commit"
    );
    // Native tools report errors to the model; assert the commits exist in a
    // later command so a scripted final response cannot hide a failed tool.
    let verify = format!(
        "set -eu; {check_env}; git log --format=%s | grep -Fx command-commit; \
         git log --format=%s | grep -Fx agent-commit; \
         git log --format=%s | grep -F 'fabro('; \
         test \"$(git log --format='%an|%ae|%cn|%ce' --extended-regexp --grep='^(fabro\\(|command-commit$|agent-commit$)' | sort -u)\" = {expected}; \
         test \"$(git -C extra log -1 --format='%an|%ae|%cn|%ce')\" = {expected}; \
         ! git config --get user.name; ! git config --get user.email; \
         ! git -C extra config --get user.name; ! git -C extra config --get user.email"
    );
    // JSON quoting is also valid DOT string quoting for these shell scripts.
    let workflow = format!(
        r#"digraph Identity {{
    graph [goal="Verify commit identities", backend="api", default_max_retries=0]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    command [shape=parallelogram, goal_gate=true, script={}]
    agent [shape=box, prompt="Create commits", model="test/model"]
    verify [shape=parallelogram, goal_gate=true, script={}]
    start -> command -> agent -> verify -> exit
}}"#,
        serde_json::to_string(&command).expect("command quoting"),
        serde_json::to_string(&verify).expect("verify quoting"),
    );
    let (client, scripted) = test_support::scripted_client(vec![
        ScriptedCall::response(test_support::tool_call_response(
            "shell",
            "commit",
            json!({"command":agent}),
        )),
        ScriptedCall::response(test_support::text_response("Created commits.")),
    ]);
    let runtime = RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    };
    let graphs = support::admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", &settings)],
        Launch::default(),
        &runtime,
    );
    let root = tempfile::tempdir().expect("isolated run");
    let run_id = RunId::new().to_string();
    let store = Arc::new(MemoryRunStore::new());
    let mut request = support::run_request(
        &run_id,
        &root.path().join("run"),
        graphs,
        store.clone() as Arc<dyn RunStore>,
        runtime,
        support::no_questions(Arc::new(support::Silent)),
    );
    request.provider = provider.clone();
    let mut hooks = HooksSpec::for_run(
        Arc::new(MemoryPlatformRecords::new()),
        &namespace,
        Arc::new(StoreArtifactWriter::new(ArtifactStore::new(
            Arc::new(InMemory::new()),
            "identity-test",
        ))),
    );
    hooks.git.host_workspaces = provider == SandboxProviderKind::LOCAL;
    if provider == SandboxProviderKind::DOCKER {
        // Remote sandboxes checkpoint only Git-backed runs. Use the same
        // public fixture as the existing Docker checkpoint integration test.
        hooks.source = Some(RunSource {
            origin:      "https://github.com/octocat/Hello-World.git".into(),
            revision:    SourceRevision::Branch("master".into()),
            branch:      "master".into(),
            depth:       Some(1),
            credentials: None,
        });
    }
    request.hooks = Some(hooks);
    request.blobs = Some(Arc::new(MemoryBlobs::new()));
    let outcome = engine::run(request).await.expect("run executes");
    if provider == SandboxProviderKind::DOCKER {
        prune::prune(PruneRequest {
            store: store.clone(),
            run_id: run_id.clone(),
            sandbox: SandboxProviderConfig::default(),
            run_dir: root.path().join("run"),
            provider,
        })
        .await
        .expect("test sandboxes pruned");
    }
    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert_eq!(
        scripted.requests().len(),
        2,
        "the native agent executed its tool"
    );
}

#[tokio::test]
async fn configured_identity_reaches_commands_agents_and_checkpoints() {
    commits_share_identity(
        SandboxProviderKind::LOCAL,
        Some("Run O'Author"),
        Some("run@example.com"),
    )
    .await;
}

#[tokio::test]
async fn default_identity_reaches_commands_agents_and_checkpoints() {
    commits_share_identity(SandboxProviderKind::LOCAL, None, None).await;
}

#[tokio::test]
async fn partial_identity_reaches_commands_agents_and_checkpoints() {
    commits_share_identity(SandboxProviderKind::LOCAL, Some("Run O'Author"), None).await;
}

#[fabro_macros::e2e_test()]
async fn clean_docker_commands_agents_and_checkpoints_share_identity() {
    commits_share_identity(
        SandboxProviderKind::DOCKER,
        Some("Run O'Author"),
        Some("run@example.com"),
    )
    .await;
}
