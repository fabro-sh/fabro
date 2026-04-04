use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_dry_run, setup_detached_dry_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["logs", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    View the event log of a workflow run

    Usage: fabro logs [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name (most recent run)

    Options:
      -f, --follow                     Follow log output
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --since <SINCE>              Logs since timestamp or relative (e.g. "42m", "2h", "2026-01-02T13:00:00Z")
      -n, --tail <TAIL>                Lines from end (default: all)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -p, --pretty                     Formatted colored output with rendered assistant text
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --server-url <SERVER_URL>    Server URL (overrides server.base_url from user.toml) [env: FABRO_SERVER_URL=]
      -h, --help                       Print help
    ----- stderr -----
    "#);
}

#[test]
fn logs_completed_run_outputs_raw_ndjson() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":"(?:\[DRY_RUN_DIR\]|\[STORAGE_DIR\]/runs/REDACTED)""#.to_string(),
        r#""run_dir":"[RUN_DIR]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {"event":"run.created","id":"[EVENT_ID]","properties":{"graph":{"attrs":{"goal":{"String":"Run tests and report results"},"rankdir":{"String":"LR"}},"edges":[{"attrs":{},"from":"start","to":"run_tests"},{"attrs":{},"from":"run_tests","to":"report"},{"attrs":{},"from":"report","to":"exit"}],"name":"Simple","nodes":{"exit":{"attrs":{"label":{"String":"Exit"},"shape":{"String":"Msquare"}},"id":"exit"},"report":{"attrs":{"label":{"String":"Report"},"prompt":{"String":"Summarize the test results"}},"id":"report"},"run_tests":{"attrs":{"label":{"String":"Run Tests"},"prompt":{"String":"Run the test suite and report results"}},"id":"run_tests"},"start":{"attrs":{"label":{"String":"Start"},"shape":{"String":"Mdiamond"}},"id":"start"}}},"host_repo_path":"[TEMP_DIR]","run_dir":"[STORAGE_DIR]/runs/20260404-[ULID]","settings":{"auto_approve":true,"dry_run":true,"fabro":{"root":"fabro/"},"features":{"retros":false,"session_sandboxes":false},"goal":"Run tests and report results","hooks":[{"blocking":true,"command":"cargo fmt","event":"post_tool_use","matcher":"write_file|edit_file|apply_patch","name":"cargo-fmt","sandbox":null,"timeout_ms":null}],"llm":{"fallbacks":null,"model":"claude-sonnet-4-6","provider":"anthropic"},"mode":"standalone","no_retro":true,"pull_request":{"auto_merge":false,"draft":false,"enabled":true,"merge_strategy":"squash"},"sandbox":{"daytona":{"auto_stop_interval":30,"labels":{"repo":"fabro-sh/fabro"},"network":null,"skip_clone":false,"snapshot":{"cpu":4,"disk":20,"dockerfile":"FROM ubuntu:24.04/n/nRUN apt-get update && apt-get install -y --no-install-recommends curl git ca-certificates build-essential pkg-config libssl-dev unzip python3 && rm -rf /var/lib/apt/lists/*/n/n# GitHub CLI/nRUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && echo \"deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*/n/n# Rust/nRUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y/nENV PATH=\"/root/.cargo/bin:${PATH}\"/nRUN cargo install cargo-nextest --locked/nENV CARGO_INCREMENTAL=0/n/n# Bun/nRUN curl -fsSL https://bun.sh/install | bash/nENV PATH=\"/root/.bun/bin:${PATH}\"/n/nWORKDIR /root/n","memory":8,"name":"fabro-v6"}},"devcontainer":null,"env":null,"local":null,"preserve":null,"provider":"local"},"storage_dir":"[STORAGE_DIR]","version":1},"workflow_slug":"simple","workflow_source":"digraph Simple {/n    graph [goal=\"Run tests and report results\"]/n    rankdir=LR/n/n    start [shape=Mdiamond, label=\"Start\"]/n    exit  [shape=Msquare, label=\"Exit\"]/n/n    run_tests [label=\"Run Tests\", prompt=\"Run the test suite and report results\"]/n    report    [label=\"Report\", prompt=\"Summarize the test results\"]/n/n    start -> run_tests -> report -> exit/n}/n","working_directory":"[TEMP_DIR]"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.submitted","id":"[EVENT_ID]","properties":{},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.starting","id":"[EVENT_ID]","properties":{"reason":"sandbox_initializing"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.initializing","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.ready","id":"[EVENT_ID]","properties":{"duration_ms": [DURATION_MS],"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.initialized","id":"[EVENT_ID]","properties":{"provider":"local","working_directory":"[TEMP_DIR]"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.started","id":"[EVENT_ID]","properties":{"goal":"Run tests and report results","name":"Simple"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.running","id":"[EVENT_ID]","properties":{},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"start","node_label":"Start","properties":{"attempt":1,"handler_type":"start","index":0,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"start","node_label":"Start","properties":{"attempt":1,"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"start","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.run_id":"[ULID]","internal.thread_id":null},"duration_ms": [DURATION_MS],"index":0,"max_attempts":1,"node_visits":{"start":1},"notes":"[Simulated] start","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"edge.selected","id":"[EVENT_ID]","properties":{"from_node":"start","is_jump":false,"reason":"unconditional","stage_status":"success","to_node":"run_tests"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"checkpoint.completed","id":"[EVENT_ID]","node_id":"start","node_label":"start","properties":{"completed_nodes":["start"],"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"start","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":null,"outcome":"success"},"current_node":"start","next_node_id":"run_tests","node_outcomes":{"start":{"notes":"[Simulated] start","status":"success","usage":null}},"node_visits":{"start":1},"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"run_tests","node_label":"Run Tests","properties":{"attempt":1,"handler_type":"agent","index":1,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"run_tests","node_label":"Run Tests","properties":{"attempt":1,"context_updates":{"last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","response.run_tests":"[Simulated] Response for stage: run_tests"},"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"run_tests","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"start","outcome":"success","thread.start.current_node":"run_tests"},"duration_ms": [DURATION_MS],"index":1,"max_attempts":1,"node_visits":{"run_tests":1,"start":1},"notes":"[Simulated] run_tests","response":"[Simulated] Response for stage: run_tests","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"edge.selected","id":"[EVENT_ID]","properties":{"from_node":"run_tests","is_jump":false,"reason":"unconditional","stage_status":"success","to_node":"report"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"checkpoint.completed","id":"[EVENT_ID]","node_id":"run_tests","node_label":"run_tests","properties":{"completed_nodes":["start","run_tests"],"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"run_tests","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.run_tests":0,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"start","last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","outcome":"success","response.run_tests":"[Simulated] Response for stage: run_tests","thread.start.current_node":"run_tests"},"current_node":"run_tests","next_node_id":"report","node_outcomes":{"run_tests":{"context_updates":{"last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","response.run_tests":"[Simulated] Response for stage: run_tests"},"notes":"[Simulated] run_tests","status":"success","usage":null},"start":{"notes":"[Simulated] start","status":"success","usage":null}},"node_visits":{"run_tests":1,"start":1},"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"report","node_label":"Report","properties":{"attempt":1,"handler_type":"agent","index":2,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"report","node_label":"Report","properties":{"attempt":1,"context_updates":{"last_response":"[Simulated] Response for stage: report","last_stage":"report","response.report":"[Simulated] Response for stage: report"},"context_values":{"current.preamble":"Goal: Run tests and report results/n/n## Completed stages/n- **run_tests**: success/n","current_node":"report","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.run_tests":0,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"run_tests","last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","outcome":"success","response.run_tests":"[Simulated] Response for stage: run_tests","thread.run_tests.current_node":"report","thread.start.current_node":"run_tests"},"duration_ms": [DURATION_MS],"index":2,"max_attempts":1,"node_visits":{"report":1,"run_tests":1,"start":1},"notes":"[Simulated] report","response":"[Simulated] Response for stage: report","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"edge.selected","id":"[EVENT_ID]","properties":{"from_node":"report","is_jump":false,"reason":"unconditional","stage_status":"success","to_node":"exit"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"checkpoint.completed","id":"[EVENT_ID]","node_id":"report","node_label":"report","properties":{"completed_nodes":["start","run_tests","report"],"context_values":{"current.preamble":"Goal: Run tests and report results/n/n## Completed stages/n- **run_tests**: success/n","current_node":"report","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.report":0,"internal.retry_count.run_tests":0,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"run_tests","last_response":"[Simulated] Response for stage: report","last_stage":"report","outcome":"success","response.report":"[Simulated] Response for stage: report","response.run_tests":"[Simulated] Response for stage: run_tests","thread.run_tests.current_node":"report","thread.start.current_node":"run_tests"},"current_node":"report","next_node_id":"exit","node_outcomes":{"report":{"context_updates":{"last_response":"[Simulated] Response for stage: report","last_stage":"report","response.report":"[Simulated] Response for stage: report"},"notes":"[Simulated] report","status":"success","usage":null},"run_tests":{"context_updates":{"last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","response.run_tests":"[Simulated] Response for stage: run_tests"},"notes":"[Simulated] run_tests","status":"success","usage":null},"start":{"notes":"[Simulated] start","status":"success","usage":null}},"node_visits":{"report":1,"run_tests":1,"start":1},"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"exit","node_label":"Exit","properties":{"attempt":1,"handler_type":"exit","index":3,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"exit","node_label":"Exit","properties":{"attempt":1,"duration_ms": [DURATION_MS],"index":3,"max_attempts":1,"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.completed","id":"[EVENT_ID]","properties":{"artifact_count":0,"duration_ms": [DURATION_MS],"reason":"completed","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.cleanup.started","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.cleanup.completed","id":"[EVENT_ID]","properties":{"duration_ms": [DURATION_MS],"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    ----- stderr -----
    "#);
}

#[test]
fn logs_completed_run_reads_store_without_progress_jsonl() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let _ = std::fs::remove_file(run.run_dir.join("progress.jsonl"));

    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":"(?:\[DRY_RUN_DIR\]|\[STORAGE_DIR\]/runs/REDACTED)""#.to_string(),
        r#""run_dir":"[RUN_DIR]""#.to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["logs", "--tail", "2", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {"event":"sandbox.cleanup.started","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.cleanup.completed","id":"[EVENT_ID]","properties":{"duration_ms": [DURATION_MS],"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    ----- stderr -----
    "#);
}

#[test]
fn logs_tail_limits_output() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":"(?:\[DRY_RUN_DIR\]|\[STORAGE_DIR\]/runs/REDACTED)""#.to_string(),
        r#""run_dir":"[RUN_DIR]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--tail", "2", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {"event":"sandbox.cleanup.started","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.cleanup.completed","id":"[EVENT_ID]","properties":{"duration_ms": [DURATION_MS],"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    ----- stderr -----
    "#);
}

#[test]
fn logs_pretty_formats_small_run() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((r"\b\d{2}:\d{2}:\d{2}\b".to_string(), "[CLOCK]".to_string()));
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--pretty", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [CLOCK]   Sandbox: local  [DURATION]
    [CLOCK] ▶ Simple  [ULID]
                Run tests and report results

    [CLOCK] ▶ Start
    [CLOCK] ✓ Start    [DURATION]
    [CLOCK]    → run_tests unconditional
    [CLOCK] ▶ Run Tests
    [CLOCK] ✓ Run Tests    [DURATION]
    [CLOCK]    → report unconditional
    [CLOCK] ▶ Report
    [CLOCK] ✓ Report    [DURATION]
    [CLOCK]    → exit unconditional
    [CLOCK] ▶ Exit
    [CLOCK] ✓ Exit    [DURATION]
    [CLOCK] ✓ SUCCESS [DURATION]  
    ----- stderr -----
    "#);
}

#[test]
fn logs_follow_detached_run_streams_until_completion() {
    let context = test_context!();
    let run = setup_detached_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":"(?:\[DRY_RUN_DIR\]|\[STORAGE_DIR\]/runs/REDACTED)""#.to_string(),
        r#""run_dir":"[RUN_DIR]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--follow", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {"event":"run.created","id":"[EVENT_ID]","properties":{"graph":{"attrs":{"goal":{"String":"Run tests and report results"},"rankdir":{"String":"LR"}},"edges":[{"attrs":{},"from":"start","to":"run_tests"},{"attrs":{},"from":"run_tests","to":"report"},{"attrs":{},"from":"report","to":"exit"}],"name":"Simple","nodes":{"exit":{"attrs":{"label":{"String":"Exit"},"shape":{"String":"Msquare"}},"id":"exit"},"report":{"attrs":{"label":{"String":"Report"},"prompt":{"String":"Summarize the test results"}},"id":"report"},"run_tests":{"attrs":{"label":{"String":"Run Tests"},"prompt":{"String":"Run the test suite and report results"}},"id":"run_tests"},"start":{"attrs":{"label":{"String":"Start"},"shape":{"String":"Mdiamond"}},"id":"start"}}},"host_repo_path":"[TEMP_DIR]","run_dir":"[STORAGE_DIR]/runs/20260404-[ULID]","settings":{"auto_approve":true,"dry_run":true,"fabro":{"root":"fabro/"},"features":{"retros":false,"session_sandboxes":false},"goal":"Run tests and report results","hooks":[{"blocking":true,"command":"cargo fmt","event":"post_tool_use","matcher":"write_file|edit_file|apply_patch","name":"cargo-fmt","sandbox":null,"timeout_ms":null}],"llm":{"fallbacks":null,"model":"claude-sonnet-4-6","provider":"anthropic"},"mode":"standalone","no_retro":true,"pull_request":{"auto_merge":false,"draft":false,"enabled":true,"merge_strategy":"squash"},"sandbox":{"daytona":{"auto_stop_interval":30,"labels":{"repo":"fabro-sh/fabro"},"network":null,"skip_clone":false,"snapshot":{"cpu":4,"disk":20,"dockerfile":"FROM ubuntu:24.04/n/nRUN apt-get update && apt-get install -y --no-install-recommends curl git ca-certificates build-essential pkg-config libssl-dev unzip python3 && rm -rf /var/lib/apt/lists/*/n/n# GitHub CLI/nRUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg && echo \"deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null && apt-get update && apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*/n/n# Rust/nRUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y/nENV PATH=\"/root/.cargo/bin:${PATH}\"/nRUN cargo install cargo-nextest --locked/nENV CARGO_INCREMENTAL=0/n/n# Bun/nRUN curl -fsSL https://bun.sh/install | bash/nENV PATH=\"/root/.bun/bin:${PATH}\"/n/nWORKDIR /root/n","memory":8,"name":"fabro-v6"}},"devcontainer":null,"env":null,"local":null,"preserve":null,"provider":"local"},"storage_dir":"[STORAGE_DIR]","version":1},"workflow_slug":"simple","workflow_source":"digraph Simple {/n    graph [goal=\"Run tests and report results\"]/n    rankdir=LR/n/n    start [shape=Mdiamond, label=\"Start\"]/n    exit  [shape=Msquare, label=\"Exit\"]/n/n    run_tests [label=\"Run Tests\", prompt=\"Run the test suite and report results\"]/n    report    [label=\"Report\", prompt=\"Summarize the test results\"]/n/n    start -> run_tests -> report -> exit/n}/n","working_directory":"[TEMP_DIR]"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.submitted","id":"[EVENT_ID]","properties":{},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.starting","id":"[EVENT_ID]","properties":{"reason":"sandbox_initializing"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.initializing","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.ready","id":"[EVENT_ID]","properties":{"duration_ms": [DURATION_MS],"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.initialized","id":"[EVENT_ID]","properties":{"provider":"local","working_directory":"[TEMP_DIR]"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.started","id":"[EVENT_ID]","properties":{"goal":"Run tests and report results","name":"Simple"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.running","id":"[EVENT_ID]","properties":{},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"start","node_label":"Start","properties":{"attempt":1,"handler_type":"start","index":0,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"start","node_label":"Start","properties":{"attempt":1,"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"start","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.run_id":"[ULID]","internal.thread_id":null},"duration_ms": [DURATION_MS],"index":0,"max_attempts":1,"node_visits":{"start":1},"notes":"[Simulated] start","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"edge.selected","id":"[EVENT_ID]","properties":{"from_node":"start","is_jump":false,"reason":"unconditional","stage_status":"success","to_node":"run_tests"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"checkpoint.completed","id":"[EVENT_ID]","node_id":"start","node_label":"start","properties":{"completed_nodes":["start"],"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"start","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":null,"outcome":"success"},"current_node":"start","next_node_id":"run_tests","node_outcomes":{"start":{"notes":"[Simulated] start","status":"success","usage":null}},"node_visits":{"start":1},"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"run_tests","node_label":"Run Tests","properties":{"attempt":1,"handler_type":"agent","index":1,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"run_tests","node_label":"Run Tests","properties":{"attempt":1,"context_updates":{"last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","response.run_tests":"[Simulated] Response for stage: run_tests"},"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"run_tests","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"start","outcome":"success","thread.start.current_node":"run_tests"},"duration_ms": [DURATION_MS],"index":1,"max_attempts":1,"node_visits":{"run_tests":1,"start":1},"notes":"[Simulated] run_tests","response":"[Simulated] Response for stage: run_tests","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"edge.selected","id":"[EVENT_ID]","properties":{"from_node":"run_tests","is_jump":false,"reason":"unconditional","stage_status":"success","to_node":"report"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"checkpoint.completed","id":"[EVENT_ID]","node_id":"run_tests","node_label":"run_tests","properties":{"completed_nodes":["start","run_tests"],"context_values":{"current.preamble":"Goal: Run tests and report results/n","current_node":"run_tests","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.run_tests":0,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"start","last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","outcome":"success","response.run_tests":"[Simulated] Response for stage: run_tests","thread.start.current_node":"run_tests"},"current_node":"run_tests","next_node_id":"report","node_outcomes":{"run_tests":{"context_updates":{"last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","response.run_tests":"[Simulated] Response for stage: run_tests"},"notes":"[Simulated] run_tests","status":"success","usage":null},"start":{"notes":"[Simulated] start","status":"success","usage":null}},"node_visits":{"run_tests":1,"start":1},"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"report","node_label":"Report","properties":{"attempt":1,"handler_type":"agent","index":2,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"report","node_label":"Report","properties":{"attempt":1,"context_updates":{"last_response":"[Simulated] Response for stage: report","last_stage":"report","response.report":"[Simulated] Response for stage: report"},"context_values":{"current.preamble":"Goal: Run tests and report results/n/n## Completed stages/n- **run_tests**: success/n","current_node":"report","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.run_tests":0,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"run_tests","last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","outcome":"success","response.run_tests":"[Simulated] Response for stage: run_tests","thread.run_tests.current_node":"report","thread.start.current_node":"run_tests"},"duration_ms": [DURATION_MS],"index":2,"max_attempts":1,"node_visits":{"report":1,"run_tests":1,"start":1},"notes":"[Simulated] report","response":"[Simulated] Response for stage: report","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"edge.selected","id":"[EVENT_ID]","properties":{"from_node":"report","is_jump":false,"reason":"unconditional","stage_status":"success","to_node":"exit"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"checkpoint.completed","id":"[EVENT_ID]","node_id":"report","node_label":"report","properties":{"completed_nodes":["start","run_tests","report"],"context_values":{"current.preamble":"Goal: Run tests and report results/n/n## Completed stages/n- **run_tests**: success/n","current_node":"report","failure_class":"","failure_signature":"","graph.goal":"Run tests and report results","graph.rankdir":"LR","internal.fidelity":"compact","internal.node_visit_count":1,"internal.retry_count.report":0,"internal.retry_count.run_tests":0,"internal.retry_count.start":0,"internal.run_id":"[ULID]","internal.thread_id":"run_tests","last_response":"[Simulated] Response for stage: report","last_stage":"report","outcome":"success","response.report":"[Simulated] Response for stage: report","response.run_tests":"[Simulated] Response for stage: run_tests","thread.run_tests.current_node":"report","thread.start.current_node":"run_tests"},"current_node":"report","next_node_id":"exit","node_outcomes":{"report":{"context_updates":{"last_response":"[Simulated] Response for stage: report","last_stage":"report","response.report":"[Simulated] Response for stage: report"},"notes":"[Simulated] report","status":"success","usage":null},"run_tests":{"context_updates":{"last_response":"[Simulated] Response for stage: run_tests","last_stage":"run_tests","response.run_tests":"[Simulated] Response for stage: run_tests"},"notes":"[Simulated] run_tests","status":"success","usage":null},"start":{"notes":"[Simulated] start","status":"success","usage":null}},"node_visits":{"report":1,"run_tests":1,"start":1},"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.started","id":"[EVENT_ID]","node_id":"exit","node_label":"Exit","properties":{"attempt":1,"handler_type":"exit","index":3,"max_attempts":1},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"stage.completed","id":"[EVENT_ID]","node_id":"exit","node_label":"Exit","properties":{"attempt":1,"duration_ms": [DURATION_MS],"index":3,"max_attempts":1,"status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"run.completed","id":"[EVENT_ID]","properties":{"artifact_count":0,"duration_ms": [DURATION_MS],"reason":"completed","status":"success"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.cleanup.started","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"event":"sandbox.cleanup.completed","id":"[EVENT_ID]","properties":{"duration_ms": [DURATION_MS],"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    ----- stderr -----
    "#);
}
