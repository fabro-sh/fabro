#![allow(
    clippy::absolute_paths,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    reason = "These workflow-hook tests value explicit fixtures over pedantic style lints."
)]
#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures with sync std::fs; test infrastructure, not Tokio-hot path"
)]

use std::process::Output;

use fabro_auth::{AuthCredential, AuthDetails};
use fabro_config::Storage;
use fabro_model::Provider;
use fabro_test::{
    TestMode, TwinOpenAi, TwinScenario, TwinScenarios, TwinToolCall, test_context, twin_openai,
};
use fabro_vault::{SecretType, Vault};

use super::read_conclusion;

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

async fn run_failure_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().failure().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

fn hook_model() -> &'static str {
    if TestMode::from_env().is_twin() {
        "gpt-5.4-mini"
    } else {
        "haiku"
    }
}

fn stage_model() -> &'static str {
    if TestMode::from_env().is_twin() {
        "gpt-5.4-mini"
    } else {
        "claude-haiku-4-5"
    }
}

fn stage_provider() -> &'static str {
    if TestMode::from_env().is_twin() {
        "openai"
    } else {
        "anthropic"
    }
}

fn toml_path(path: &std::path::Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn twin_server_storage_dir(context: &fabro_test::TestContext) -> std::path::PathBuf {
    context.temp_dir.join("hook-server-storage")
}

fn settings_with_hook(context: &fabro_test::TestContext, hook: &str) -> String {
    if TestMode::from_env().is_twin() {
        format!(
            r#"[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token"]

{hook}"#,
            toml_path(&twin_server_storage_dir(context)),
        )
    } else {
        hook.to_string()
    }
}

fn write_hook_settings(context: &fabro_test::TestContext, hook: &str) {
    let settings = settings_with_hook(context, hook);
    if settings.trim().is_empty() {
        return;
    }
    context.write_home(".fabro/settings.toml", settings);
}

fn seed_openai_vault(storage_dir: &std::path::Path, base_url: &str, api_key: &str) {
    let mut vault =
        Vault::load(Storage::new(storage_dir).secrets_path()).expect("test vault should load");
    vault
        .set(
            "openai",
            &serde_json::to_string(&AuthCredential {
                provider: Provider::OpenAi,
                details:  AuthDetails::ApiKey {
                    key: api_key.to_string(),
                },
            })
            .expect("OpenAI test credential should serialize"),
            SecretType::Credential,
            None,
        )
        .expect("OpenAI credential should store in test vault");
    vault
        .set("OPENAI_BASE_URL", base_url, SecretType::Environment, None)
        .expect("OpenAI base URL should store in test vault");
}

fn configure_twin_server(
    context: &mut fabro_test::TestContext,
    twin: &TwinOpenAi,
    namespace: &str,
) {
    seed_openai_vault(&twin_server_storage_dir(context), &twin.base_url, namespace);
    context.isolated_server();
}

fn write_workflow(context: &fabro_test::TestContext, name: &str, dot: &str) -> std::path::PathBuf {
    context.write_temp(name, dot);
    context.temp_dir.join(name)
}

fn configure_hook_env(cmd: &mut assert_cmd::Command, hook_model: &str) {
    cmd.env_remove("CHATGPT_ACCOUNT_ID");
    cmd.env_remove("OPENAI_ORG_ID");
    cmd.env_remove("OPENAI_PROJECT_ID");
    if TestMode::from_env().is_twin() {
        cmd.env_remove("ANTHROPIC_API_KEY");
    }
    cmd.arg("--sandbox").arg("local");
    cmd.arg("--auto-approve");
    cmd.arg("--provider").arg(stage_provider());
    cmd.arg("--model").arg(hook_model);
}

async fn conclusion_status(context: &fabro_test::TestContext) -> String {
    let run_dir = context.single_run_dir();
    tokio::task::spawn_blocking(move || {
        read_conclusion(&run_dir)["status"]
            .as_str()
            .expect("conclusion should include a string status")
            .to_string()
    })
    .await
    .expect("conclusion status task should complete")
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_prompt_proceed_allows_run() {
    let mut context = test_context!();
    write_hook_settings(
        &context,
        &format!(
            r#"
[[run.hooks]]
name = "prompt-proceed"
event = "run_start"
prompt = "A workflow is starting. Always approve. Respond with {{\"ok\": true}}."
model = "{model}"
"#,
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_prompt_proceed.fabro",
        r"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }",
    );

    if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(TwinScenario::responses("gpt-5.4-mini").text(r#"{"ok":true}"#))
            .load(twin)
            .await;
        configure_twin_server(&mut context, twin, &namespace);
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        twin.configure_command(&mut cmd, &namespace);
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    } else {
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    }

    assert_eq!(conclusion_status(&context).await, "succeeded");
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_prompt_block_prevents_run() {
    let mut context = test_context!();
    write_hook_settings(
        &context,
        &format!(
            r#"
[[run.hooks]]
name = "prompt-block"
event = "run_start"
prompt = "Check: is 2+2 equal to 5? If the statement is true, respond {{\"ok\": true}}. If false, respond {{\"ok\": false, \"reason\": \"math check failed\"}}."
model = "{model}"
"#,
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_prompt_block.fabro",
        r"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }",
    );

    let output = if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .text(r#"{"ok":false,"reason":"math check failed"}"#),
            )
            .load(twin)
            .await;
        configure_twin_server(&mut context, twin, &namespace);
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        twin.configure_command(&mut cmd, &namespace);
        cmd.arg(&workflow);
        run_failure_output(cmd).await
    } else {
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        cmd.arg(&workflow);
        run_failure_output(cmd).await
    };

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("math check failed"),
        "stderr should include hook block reason, got: {stderr}"
    );
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_agent_proceed_allows_run() {
    let mut context = test_context!();
    write_hook_settings(
        &context,
        &format!(
            r#"
[[run.hooks]]
name = "agent-proceed"
event = "run_start"
prompt = "A workflow is starting. Always approve. Respond with {{\"ok\": true}}. Do not use any tools."
model = "{model}"
max_tool_rounds = 1
agent = "enabled"
"#,
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_agent_proceed.fabro",
        r"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }",
    );

    if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(TwinScenario::responses("gpt-5.4-mini").text(r#"{"ok":true}"#))
            .load(twin)
            .await;
        configure_twin_server(&mut context, twin, &namespace);
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        twin.configure_command(&mut cmd, &namespace);
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    } else {
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    }

    assert_eq!(conclusion_status(&context).await, "succeeded");
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_agent_with_tool_use() {
    let mut context = test_context!();
    let marker = context.temp_dir.join("hook_check.txt");
    std::fs::write(&marker, "READY").unwrap();
    write_hook_settings(
        &context,
        &format!(
            r#"
[[run.hooks]]
name = "agent-tools"
event = "run_start"
prompt = "Read the file at {path} using the read_file tool. If it contains 'READY', respond with {{\"ok\": true}}. Otherwise respond with {{\"ok\": false, \"reason\": \"not ready\"}}."
model = "{model}"
max_tool_rounds = 5
agent = "enabled"
"#,
            path = marker.display(),
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_agent_tools.fabro",
        r"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }",
    );

    if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .tool_call(TwinToolCall::read_file(marker.display().to_string())),
            )
            .scenario(TwinScenario::responses("gpt-5.4-mini").text(r#"{"ok":true}"#))
            .load(twin)
            .await;
        configure_twin_server(&mut context, twin, &namespace);
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        twin.configure_command(&mut cmd, &namespace);
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    } else {
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    }

    assert_eq!(conclusion_status(&context).await, "succeeded");
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn arc_e2e_with_real_llm() {
    let mut context = test_context!();
    write_hook_settings(&context, "");
    let hello = context.temp_dir.join("hello.txt");
    let workflow = write_workflow(
        &context,
        "arc_e2e_real_llm.fabro",
        &format!(
            r#"digraph E2E {{
                graph [goal="Create a test file"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                work  [
                    shape=box,
                    label="Work",
                    prompt="Create a file called hello.txt in {} containing exactly 'Hello from LLM'. Do not output anything else.",
                    goal_gate=true
                ]
                start -> work -> exit
            }}"#,
            context.temp_dir.display()
        ),
    );

    if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(
                TwinScenario::responses("gpt-5.4-mini")
                    .input_contains("Create a file called hello.txt")
                    .tool_call(TwinToolCall::write_file(
                        hello.display().to_string(),
                        "Hello from LLM",
                    ))
                    .text("Done."),
            )
            .load(twin)
            .await;
        configure_twin_server(&mut context, twin, &namespace);
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        twin.configure_command(&mut cmd, &namespace);
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    } else {
        let mut cmd = context.run_cmd();
        configure_hook_env(&mut cmd, stage_model());
        cmd.arg(&workflow);
        run_success_output(cmd).await;
    }

    assert_eq!(
        std::fs::read_to_string(&hello).unwrap(),
        "Hello from LLM",
        "workflow should create the expected file"
    );
    assert_eq!(conclusion_status(&context).await, "succeeded");
}
