#![expect(
    clippy::disallowed_methods,
    reason = "integration test initializes an isolated git repository with the system git binary"
)]

use fabro_acp::test_support::fake_acp_agent_script;
use fabro_auth::{AuthCredential, AuthDetails};
use fabro_config::Storage;
use fabro_model::Provider;
use fabro_test::test_context;
use fabro_types::EventBody;
use fabro_vault::{SecretType, Vault};

use super::{find_run_dir, has_event, read_conclusion, run_events, run_state};

#[test]
fn acp_backend_workflow() {
    let mut context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    context.isolated_server();
    seed_openai_vault(&context.storage_dir);
    let fake_agent = write_fake_acp_agent(&context);
    let acp_command = fake_acp_command_attr(&fake_agent);
    let workflow = context.temp_dir.join("acp_backend.fabro");
    context.write_temp(
        "acp_backend.fabro",
        format!(
            r#"digraph ACP {{
  graph [goal="Exercise ACP backend"]
  start [shape=Mdiamond]
  work [type="agent", backend="acp", provider="openai", model="fake-acp", prompt="write hello.txt", acp_command={acp_command}]
  exit [shape=Msquare]
  start -> work
  work -> exit
}}"#
        ),
    );
    init_git_repo(&context.temp_dir);

    context
        .run_cmd()
        .args(["--auto-approve", "--sandbox", "local"])
        .arg(&workflow)
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("succeeded"));

    let events = run_events(&run_dir);
    assert!(has_event(&run_dir, "agent.acp.started"));
    assert!(has_event(&run_dir, "agent.acp.completed"));
    let completed = events
        .iter()
        .find_map(|event| match &event.event.body {
            EventBody::StageCompleted(props) if event.event.node_id.as_deref() == Some("work") => {
                Some(props)
            }
            _ => None,
        })
        .expect("work stage should complete");
    assert_eq!(completed.response.as_deref(), Some("hello from acp"));
    assert!(
        completed
            .files_touched
            .iter()
            .any(|file| file == "hello.txt"),
        "files_touched should include hello.txt: {:?}",
        completed.files_touched
    );

    let state = serde_json::to_value(run_state(&run_dir)).expect("run state should serialize");
    let stages = state["stages"]
        .as_object()
        .expect("run state should contain stages");
    assert!(
        stages.values().any(|stage| {
            stage["provider_used"]["mode"] == "acp"
                && stage["provider_used"]["provider"] == "openai"
        }),
        "run projection should include ACP provider metadata: {stages:?}"
    );
}

#[test]
fn acp_prompt_workflow_uses_acp_backend() {
    let mut context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    context.isolated_server();
    seed_openai_vault(&context.storage_dir);
    let fake_agent = write_fake_acp_agent(&context);
    let acp_command = fake_acp_command_attr(&fake_agent);
    let workflow = context.temp_dir.join("acp_prompt_backend.fabro");
    context.write_temp(
        "acp_prompt_backend.fabro",
        format!(
            r#"digraph ACP {{
  graph [goal="Exercise ACP prompt backend"]
  start [shape=Mdiamond]
  prompt [type="prompt", backend="acp", provider="openai", model="fake-acp", project_memory=false, prompt="write hello.txt", acp_command={acp_command}]
  exit [shape=Msquare]
  start -> prompt
  prompt -> exit
}}"#
        ),
    );
    init_git_repo(&context.temp_dir);

    context
        .run_cmd()
        .args(["--auto-approve", "--sandbox", "local"])
        .arg(&workflow)
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("succeeded"));

    let events = run_events(&run_dir);
    assert!(has_event(&run_dir, "agent.acp.started"));
    assert!(has_event(&run_dir, "agent.acp.completed"));
    assert!(
        !has_event(&run_dir, "agent.session.activated"),
        "ACP prompt should not activate an API-mode agent session"
    );
    let completed = events
        .iter()
        .find_map(|event| match &event.event.body {
            EventBody::StageCompleted(props)
                if event.event.node_id.as_deref() == Some("prompt") =>
            {
                Some(props)
            }
            _ => None,
        })
        .expect("prompt stage should complete");
    assert_eq!(completed.response.as_deref(), Some("hello from acp"));

    let state = serde_json::to_value(run_state(&run_dir)).expect("run state should serialize");
    let stages = state["stages"]
        .as_object()
        .expect("run state should contain stages");
    assert!(
        stages.values().any(|stage| {
            stage["provider_used"]["mode"] == "acp"
                && stage["provider_used"]["provider"] == "openai"
        }),
        "run projection should include ACP provider metadata: {stages:?}"
    );
}

fn seed_openai_vault(storage_dir: &std::path::Path) {
    let mut vault =
        Vault::load(Storage::new(storage_dir).secrets_path()).expect("test vault should load");
    vault
        .set(
            "openai",
            &serde_json::to_string(&AuthCredential {
                provider: Provider::OpenAi,
                details:  AuthDetails::ApiKey {
                    key: "test-openai-key".to_string(),
                },
            })
            .expect("OpenAI test credential should serialize"),
            SecretType::Credential,
            None,
        )
        .expect("OpenAI credential should store in test vault");
}

fn write_fake_acp_agent(context: &fabro_test::TestContext) -> std::path::PathBuf {
    context.write_temp("fake_acp_agent.py", fake_acp_agent_script());
    context.temp_dir.join("fake_acp_agent.py")
}

fn fake_acp_command_attr(script_path: &std::path::Path) -> String {
    let command = serde_json::json!({
        "type": "stdio",
        "name": "fake",
        "command": "python3",
        "args": [script_path.to_string_lossy()],
        "env": [{"name": "ACP_MODE", "value": "write_file"}],
    })
    .to_string();
    format!("{command:?}")
}

fn init_git_repo(dir: &std::path::Path) {
    let output = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .output()
        .expect("git init should run");
    assert!(
        output.status.success(),
        "git init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
