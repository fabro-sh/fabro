#![expect(
    clippy::disallowed_methods,
    reason = "integration test initializes an isolated git repository with the system git binary"
)]

use fabro_acp::test_support::fake_acp_agent_script;
use fabro_config::Storage;
use fabro_test::test_context;
use fabro_types::EventBody;
use fabro_vault::{SecretType, Vault};

use super::{find_run_dir, has_event, read_conclusion, run_events, run_id_for, run_state};
use crate::cmd::support::output_stdout;

#[test]
fn acp_backend_workflow() {
    let mut context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    context.isolated_server();
    let fake_agent = write_fake_acp_agent(&context);
    let acp_config = fake_acp_config_attr(&fake_agent);
    let workflow = context.temp_dir.join("acp_backend.fabro");
    context.write_temp(
        "acp_backend.fabro",
        format!(
            r#"digraph ACP {{
  graph [goal="Exercise ACP backend"]
  start [shape=Mdiamond]
  work [type="agent", backend="acp", prompt="write hello.txt", acp.config={acp_config}]
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
                && stage["provider_used"]["config_name"] == "fake"
                && stage["provider_used"].get("provider").is_none()
        }),
        "run projection should include ACP process metadata without provider: {stages:?}"
    );
}

#[test]
fn acp_backend_does_not_inject_registered_provider_credentials() {
    let mut context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    context.isolated_server();
    seed_anthropic_vault(&context.storage_dir);

    let fake_agent = write_fake_acp_agent(&context);
    let env_record = context.temp_dir.join("acp-env.json");
    let acp_config = fake_acp_config_attr_recording_env(&fake_agent, &env_record);
    let workflow = context.temp_dir.join("acp_provider_env.fabro");
    context.write_temp(
        "acp_provider_env.fabro",
        format!(
            r#"digraph ACP {{
  graph [goal="Exercise ACP backend with stored provider credentials"]
  start [shape=Mdiamond]
  work [type="agent", backend="acp", prompt="write hello.txt", acp.config={acp_config}]
  exit [shape=Msquare]
  start -> work
  work -> exit
}}"#
        ),
    );
    init_git_repo(&context.temp_dir);

    context
        .run_cmd()
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .args(["--auto-approve", "--sandbox", "local"])
        .arg(&workflow)
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
    let conclusion = read_conclusion(&run_dir);
    assert_eq!(conclusion["status"].as_str(), Some("succeeded"));

    let recorded_env: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&env_record)
            .expect("fake ACP agent should record its environment"),
    )
    .expect("fake ACP environment record should be valid JSON");
    assert_eq!(recorded_env, serde_json::json!({}));
}

#[test]
fn acp_artifacts_are_listed_when_touched_file_mtime_precedes_attempt_start() {
    let mut context = test_context!();
    context.write_home(
        ".fabro/settings.toml",
        "[server.auth]\nmethods = [\"dev-token\"]\n",
    );
    context.isolated_server();
    let fake_agent = write_fake_acp_agent(&context);
    let acp_config = fake_acp_config_attr_with_env(&fake_agent, vec![
        serde_json::json!({
            "name": "ACP_WRITE_PATH",
            "value": "verification-artifacts/report.md",
        }),
        serde_json::json!({
            "name": "ACP_WRITE_CONTENT",
            "value": "verified\n",
        }),
        serde_json::json!({
            "name": "ACP_WRITE_MTIME_EPOCH",
            "value": "946684800",
        }),
    ]);
    context.write_temp(
        "acp_artifact_collection.fabro",
        format!(
            r#"digraph ACPArtifacts {{
  graph [goal="Capture verification artifacts"]
  start [shape=Mdiamond]
  verify [type="agent", backend="acp", prompt="write verification artifact", acp.config={acp_config}]
  exit [shape=Msquare]
  start -> verify
  verify -> exit
}}"#
        ),
    );
    context.write_temp(
        "run.toml",
        r#"_version = 1

[workflow]
graph = "acp_artifact_collection.fabro"

[run]
goal = "Capture verification artifacts"

[run.checkpoint]
exclude_globs = ["verification-artifacts/**"]

[run.artifacts]
include = ["verification-artifacts/**"]
"#,
    );
    init_git_repo(&context.temp_dir);

    context
        .run_cmd()
        .args(["--auto-approve", "--sandbox", "local"])
        .arg(context.temp_dir.join("run.toml"))
        .assert()
        .success();

    let run_dir = find_run_dir(&context);
    let run_id = run_id_for(&run_dir);
    let events = run_events(&run_dir);
    let completed = events
        .iter()
        .find_map(|event| match &event.event.body {
            EventBody::StageCompleted(props)
                if event.event.node_id.as_deref() == Some("verify") =>
            {
                Some(props)
            }
            _ => None,
        })
        .expect("verify stage should complete");
    assert!(
        completed
            .files_touched
            .iter()
            .any(|file| file == "verification-artifacts/report.md"),
        "files_touched should include the artifact path: {:?}",
        completed.files_touched
    );

    let output = context
        .command()
        .args(["--json", "artifact", "list", &run_id])
        .output()
        .expect("artifact list should execute");
    assert!(
        output.status.success(),
        "artifact list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let artifacts: Vec<serde_json::Value> =
        serde_json::from_str(&output_stdout(&output)).expect("artifact list JSON should parse");
    assert!(
        artifacts.iter().any(|artifact| {
            artifact["node_slug"] == "verify"
                && artifact["relative_path"] == "verification-artifacts/report.md"
        }),
        "artifact list should include the touched verification artifact: {artifacts:?}"
    );

    let completed_run = events
        .iter()
        .find_map(|event| match &event.event.body {
            EventBody::RunCompleted(props) => Some(props),
            _ => None,
        })
        .expect("run.completed should be emitted");
    assert!(
        completed_run.artifact_count > 0,
        "run.completed should report captured artifacts"
    );
}

fn write_fake_acp_agent(context: &fabro_test::TestContext) -> std::path::PathBuf {
    context.write_temp("fake_acp_agent.py", fake_acp_agent_script());
    context.temp_dir.join("fake_acp_agent.py")
}

fn fake_acp_config_attr(script_path: &std::path::Path) -> String {
    fake_acp_config_attr_with_env(script_path, Vec::new())
}

fn fake_acp_config_attr_recording_env(
    script_path: &std::path::Path,
    env_record: &std::path::Path,
) -> String {
    fake_acp_config_attr_with_env(script_path, vec![
        serde_json::json!({"name": "ACP_ENV_RECORD", "value": env_record.to_string_lossy()}),
        serde_json::json!({
            "name": "ACP_ENV_RECORD_KEYS",
            "value": "ANTHROPIC_API_KEY,OPENAI_API_KEY,GEMINI_API_KEY",
        }),
    ])
}

fn fake_acp_config_attr_with_env(
    script_path: &std::path::Path,
    extra_env: Vec<serde_json::Value>,
) -> String {
    let mut env = vec![serde_json::json!({"name": "ACP_MODE", "value": "write_file"})];
    env.extend(extra_env);

    let config = serde_json::json!({
        "type": "stdio",
        "name": "fake",
        "command": "python3",
        "args": [script_path.to_string_lossy()],
        "env": env,
    })
    .to_string();
    format!("{config:?}")
}

fn seed_anthropic_vault(storage_dir: &std::path::Path) {
    let mut vault =
        Vault::load(Storage::new(storage_dir).secrets_path()).expect("test vault should load");
    vault
        .set(
            "ANTHROPIC_API_KEY",
            "vault-anthropic-key",
            SecretType::Token,
            None,
        )
        .expect("Anthropic credential should store in test vault");
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
