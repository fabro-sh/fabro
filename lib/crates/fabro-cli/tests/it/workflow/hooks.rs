use std::process::Output;

use fabro_test::{TestMode, TwinScenario, TwinScenarios, TwinToolCall, test_context, twin_openai};

use super::{find_run_dir, read_conclusion};

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
    cmd.arg("--no-retro");
    cmd.arg("--auto-approve");
    cmd.arg("--provider").arg(stage_provider());
    cmd.arg("--model").arg(hook_model);
}

fn conclusion_status(context: &fabro_test::TestContext) -> String {
    let run_dir = find_run_dir(&context);
    read_conclusion(&run_dir)["status"]
        .as_str()
        .unwrap()
        .to_string()
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_prompt_proceed_allows_run() {
    let context = test_context!();
    context.write_home(
        ".fabro/user.toml",
        &format!(
            r#"
[[hooks]]
name = "prompt-proceed"
event = "run_start"
type = "prompt"
prompt = "A workflow is starting. Always approve. Respond with {{\"ok\": true}}."
model = "{model}"
"#,
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_prompt_proceed.fabro",
        r#"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }"#,
    );

    if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(TwinScenario::responses("gpt-5.4-mini").text(r#"{"ok":true}"#))
            .load(twin)
            .await;
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

    assert_eq!(conclusion_status(&context), "success");
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_prompt_block_prevents_run() {
    let context = test_context!();
    context.write_home(
        ".fabro/user.toml",
        &format!(
            r#"
[[hooks]]
name = "prompt-block"
event = "run_start"
type = "prompt"
prompt = "Check: is 2+2 equal to 5? If the statement is true, respond {{\"ok\": true}}. If false, respond {{\"ok\": false, \"reason\": \"math check failed\"}}."
model = "{model}"
"#,
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_prompt_block.fabro",
        r#"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }"#,
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
    let context = test_context!();
    context.write_home(
        ".fabro/user.toml",
        &format!(
            r#"
[[hooks]]
name = "agent-proceed"
event = "run_start"
type = "agent"
prompt = "A workflow is starting. Always approve. Respond with {{\"ok\": true}}. Do not use any tools."
model = "{model}"
max_tool_rounds = 1
"#,
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_agent_proceed.fabro",
        r#"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }"#,
    );

    if TestMode::from_env().is_twin() {
        let twin = twin_openai().await;
        let namespace = format!("{}::{}", module_path!(), line!());
        TwinScenarios::new(namespace.clone())
            .scenario(TwinScenario::responses("gpt-5.4-mini").text(r#"{"ok":true}"#))
            .load(twin)
            .await;
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

    assert_eq!(conclusion_status(&context), "success");
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn hook_agent_with_tool_use() {
    let context = test_context!();
    let marker = context.temp_dir.join("hook_check.txt");
    std::fs::write(&marker, "READY").unwrap();
    context.write_home(
        ".fabro/user.toml",
        &format!(
            r#"
[[hooks]]
name = "agent-tools"
event = "run_start"
type = "agent"
prompt = "Read the file at {path} using the read_file tool. If it contains 'READY', respond with {{\"ok\": true}}. Otherwise respond with {{\"ok\": false, \"reason\": \"not ready\"}}."
model = "{model}"
max_tool_rounds = 5
"#,
            path = marker.display(),
            model = hook_model()
        ),
    );
    let workflow = write_workflow(
        &context,
        "hook_agent_tools.fabro",
        r#"digraph HookTest {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            start -> exit
        }"#,
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

    assert_eq!(conclusion_status(&context), "success");
}

#[fabro_macros::e2e_test(twin, live("ANTHROPIC_API_KEY"))]
async fn arc_e2e_with_real_llm() {
    let context = test_context!();
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
    assert_eq!(conclusion_status(&context), "success");
}
