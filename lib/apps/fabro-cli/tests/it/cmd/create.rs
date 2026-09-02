#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage temporary workflow and Git fixtures with synchronous APIs"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fabro_test::{fabro_json_snapshot, fabro_snapshot, test_context};
use httpmock::{HttpMockResponse, Mock, MockServer};
use insta::assert_snapshot;
use serde_json::json;

use super::support::{
    created_run_id, environment_json, fixture, mock_environment,
    mock_workflow_version_registrations, mock_workflow_version_registrations_recording,
    output_stderr, output_stdout, remote_run_summary_json, resolve_run, run_count_for_test_case,
    run_git, run_state,
};
use crate::support::unique_run_id;

fn resolved_run(settings: &fabro_types::WorkflowSettings) -> fabro_types::settings::RunNamespace {
    settings.run.clone()
}

fn run_status_response(run_id: &str, status: &str) -> serde_json::Value {
    let status = match status {
        "submitted" => json!({ "kind": "submitted" }),
        other => panic!("unsupported test status {other:?}"),
    };
    remote_run_summary_json(
        run_id,
        "Test Workflow",
        "test-workflow",
        "Test run",
        &status,
        "2026-04-05T12:00:00Z",
    )
}

fn mock_intent_create<'a>(
    server: &'a MockServer,
    run_id: &str,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
) -> Mock<'a> {
    let response = run_status_response(run_id, "submitted").to_string();
    server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.respond_with(move |request| {
            requests.lock().unwrap().push(
                serde_json::from_slice(request.body_ref())
                    .expect("run-intent request body should be valid JSON"),
            );
            HttpMockResponse::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(response.clone())
                .build()
        });
    })
}

fn write_workflow(root: &std::path::Path, directory: &str, graph_name: &str) -> std::path::PathBuf {
    let directory = root.join(directory);
    std::fs::create_dir_all(&directory).expect("workflow fixture directory should be created");
    std::fs::write(
        directory.join("workflow.toml"),
        "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
    )
    .expect("workflow fixture manifest should be written");
    std::fs::write(
        directory.join("workflow.fabro"),
        format!(
            "digraph {graph_name} {{ start [shape=Mdiamond] exit [shape=Msquare] start -> exit }}"
        ),
    )
    .expect("workflow fixture graph should be written");
    directory.join("workflow.toml")
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["create", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Register a local workflow version and create a submitted run

    Usage: fabro create [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Local workflow name, checkout path, .fabro file, or workflow TOML

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --server <SERVER>            Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -I, --input <KEY=VALUE>          Override a workflow input value (repeatable, format: KEY=VALUE)
          --dry-run                    Execute with simulated LLM backend
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --auto-approve               Auto-approve all human gates
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --goal <GOAL>                Override the workflow goal (available as {{ goal }} in prompts)
          --goal-file <GOAL_FILE>      Read a per-run goal value from a local file
          --model <MODEL>              Override default LLM model
          --provider <PROVIDER>        Override default LLM provider
      -v, --verbose                    Enable verbose output
          --environment <ENVIRONMENT>  Named environment for agent tools
          --label <KEY=VALUE>          Attach a label to this run (repeatable, format: KEY=VALUE)
          --parent <RUN>               Link this run to an existing orchestration parent run
          --preserve-sandbox           Keep the sandbox alive after the run finishes (for debugging)
      -d, --detach                     Run the workflow in the background and print the run ID
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn create_uses_explicit_server_target_and_prints_remote_run_id() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&server, "default", "docker");
    let version_mock = mock_workflow_version_registrations(&server);
    let mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });

    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    environment_mock.assert();
    version_mock.assert();
    mock.assert();
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_uses_the_sole_clone_based_environment_when_default_is_missing() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let default_mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/environments/default");
        then.status(404)
            .header("content-type", "application/json")
            .json_body(json!({
                "errors": [{
                    "status": "404",
                    "title": "Not Found",
                    "detail": "environment `default` not found",
                    "code": "environment_not_found"
                }]
            }));
    });
    let list_mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/environments");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "data": [environment_json("production", "docker")],
                "meta": { "total": 1 }
            }));
    });
    let version_mock = mock_workflow_version_registrations(&server);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, Arc::clone(&requests));

    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    default_mock.assert();
    list_mock.assert();
    version_mock.assert();
    create_mock.assert();
    assert_eq!(requests.lock().unwrap()[0]["environment_id"], "production");
}

#[test]
fn create_defers_provider_validation_to_the_server() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&server, "default", "docker");
    let registered_versions = Arc::new(Mutex::new(Vec::new()));
    let version_mock =
        mock_workflow_version_registrations_recording(&server, Arc::clone(&registered_versions));
    let mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            fixture("server-model.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "local validation should not reject a server-owned provider\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    environment_mock.assert();
    version_mock.assert();
    mock.assert();
    assert!(
        serde_json::to_string(&registered_versions.lock().unwrap()[0])
            .unwrap()
            .contains("server-only")
    );
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_uses_configured_server_target_without_server_flag() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&server, "default", "docker");
    let version_mock = mock_workflow_version_registrations(&server);
    let mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    context.set_http_target(&server.base_url());

    let output = context
        .create_cmd()
        .args(["--dry-run", fixture("simple.fabro").to_str().unwrap()])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    environment_mock.assert();
    version_mock.assert();
    mock.assert();
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_parent_resolves_parent_and_sends_parent_id_in_intent() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let parent_id = unique_run_id();
    let resolve_mock = super::support::mock_resolved_run(&server, "nightly-parent", &parent_id);
    let environment_mock = mock_environment(&server, "default", "docker");
    let version_mock = mock_workflow_version_registrations(&server);
    let create_mock = server.mock(|when, then| {
        when.method("POST")
            .path("/api/v1/runs")
            .json_body_includes(format!(r#"{{"parent_id":"{parent_id}"}}"#));
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });

    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            "--parent",
            "nightly-parent",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    resolve_mock.assert();
    environment_mock.assert();
    version_mock.assert();
    create_mock.assert();
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_rejects_storage_dir_flag() {
    let context = test_context!();
    let output = context
        .create_cmd()
        .args([
            "--storage-dir",
            "/tmp/fabro-create",
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        !output.status.success(),
        "command should reject --storage-dir"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument '--storage-dir'"));
}

#[test]
fn create_cli_server_target_overrides_configured_server_target() {
    let context = test_context!();
    let config_server = MockServer::start();
    let config_mock = config_server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(500)
            .body("configured-server-should-not-be-used");
    });
    let cli_server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&cli_server, "default", "docker");
    let version_mock = mock_workflow_version_registrations(&cli_server);
    let cli_mock = cli_server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.status(201)
            .header("Content-Type", "application/json")
            .body(run_status_response(run_id.as_str(), "submitted").to_string());
    });
    context.set_http_target(&config_server.base_url());

    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", cli_server.base_url()),
            "--dry-run",
            fixture("simple.fabro").to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    environment_mock.assert();
    version_mock.assert();
    cli_mock.assert();
    config_mock.assert_calls(0);
    assert_eq!(output_stdout(&output).trim(), run_id.as_str());
}

#[test]
fn create_sends_one_sparse_typed_intent_with_local_caller_target() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let parent_id = unique_run_id();
    let environment_mock = mock_environment(&server, "local", "local");
    let version_mock = mock_workflow_version_registrations(&server);
    let resolve_mock = super::support::mock_resolved_run(&server, "parent", &parent_id);
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let create_mock = mock_intent_create(&server, &run_id, Arc::clone(&requests));
    let caller = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let workflow_dir = checkout.path().join(".fabro/workflows/exact");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        checkout.path().join(".fabro/project.toml"),
        r#"_version = 1

[run.model]
name = "project-value-must-not-cross-admission"

[environments.project-only]
provider = "local"
"#,
    )
    .unwrap();
    std::fs::write(
        workflow_dir.join("workflow.toml"),
        r#"_version = 1

[workflow]
graph = "workflow.fabro"

[run.pull_request]
enabled = true
draft = false
"#,
    )
    .unwrap();
    std::fs::write(
        workflow_dir.join("workflow.fabro"),
        r#"digraph Exact {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  task [shape=parallelogram, script="echo {{ inputs.string }}"]
  start -> task -> exit
}"#,
    )
    .unwrap();
    std::fs::write(caller.path().join("goal.md"), "Goal read from caller cwd").unwrap();
    context.write_home(
        ".fabro/settings.toml",
        r#"_version = 1

[run.model]
name = "machine-value-must-not-cross-admission"

[environments.machine-only]
provider = "local"
"#,
    );
    let workflow = workflow_dir.join("workflow.toml");
    let expected_package =
        fabro_manifest::resolve_local_workflow_package(&workflow, caller.path(), None).unwrap();
    let expected_id = expected_package.closure().root_id();

    let output = context
        .create_cmd()
        .current_dir(caller.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--environment",
            "local",
            "--parent",
            "parent",
            "--goal-file",
            "goal.md",
            "--model",
            "gpt-5",
            "--provider",
            "openai",
            "--label",
            "team=cli",
            "--input",
            "string=hello",
            "--input",
            "boolean=true",
            "--input",
            "integer=42",
            "--input",
            "float=1.25",
            "--dry-run",
            "--auto-approve",
            "--preserve-sandbox",
            "--verbose",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    environment_mock.assert();
    version_mock.assert();
    resolve_mock.assert();
    create_mock.assert_calls(1);
    let requests = requests.lock().unwrap();
    let [intent] = requests.as_slice() else {
        panic!("expected exactly one intent request: {requests:?}");
    };
    assert_eq!(intent["workflow_version_id"], expected_id.to_string());
    assert_eq!(
        intent["target"],
        json!({
            "kind": "folder",
            "path": caller.path().canonicalize().unwrap(),
        })
    );
    assert_eq!(intent["environment_id"], "local");
    assert_eq!(intent["parent_id"], parent_id.clone());
    assert_eq!(intent["goal"], "Goal read from caller cwd");
    assert_eq!(intent["args"]["model"], "gpt-5");
    assert_eq!(intent["args"]["provider"], "openai");
    assert_eq!(
        intent["args"]["inputs"],
        json!({
            "string": "hello",
            "boolean": true,
            "integer": 42,
            "float": 1.25,
        })
    );
    assert_eq!(intent["args"]["labels"]["team"], "cli");
    assert_eq!(
        intent["args"]["labels"]["fabro_test_run"],
        context.test_run_id()
    );
    assert_eq!(
        intent["args"]["labels"]["fabro_test_case"],
        context.test_case_id()
    );
    assert_eq!(intent["args"]["dry_run"], true);
    assert_eq!(intent["args"]["auto_approve"], true);
    assert_eq!(intent["args"]["preserve_sandbox"], true);
    let wire = serde_json::to_string(intent).unwrap();
    for absent in [
        "goal.md",
        "machine-value-must-not-cross-admission",
        "project-value-must-not-cross-admission",
        "workflow.toml",
        "workflow.fabro",
        "verbose",
    ] {
        assert!(!wire.contains(absent), "intent leaked {absent}: {wire}");
    }
    let stderr = output_stderr(&output);
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.contains("do not transmit these settings"))
            .count(),
        2,
        "{stderr}"
    );
    assert!(stderr.contains("contains run, environments"), "{stderr}");
    assert!(!stderr.contains("machine-value-must-not-cross-admission"));
    assert!(!stderr.contains("project-value-must-not-cross-admission"));
}

#[test]
fn create_preserves_named_user_other_checkout_and_loose_file_selection() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&server, "local", "local");
    let version_mock = mock_workflow_version_registrations(&server);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, Arc::clone(&requests));
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let other_checkout = tempfile::tempdir().unwrap();
    run_git(project.path(), &["init", "--quiet"]);
    let project_workflow = write_workflow(project.path(), ".fabro/workflows/hello", "ProjectHello");
    let user_root = context.home_dir.join(".fabro/workflows");
    let user_workflow = write_workflow(&user_root, "hello", "UserHello");
    let other_workflow = write_workflow(
        other_checkout.path(),
        ".fabro/workflows/other",
        "OtherCheckout",
    );
    let loose_workflow = outside.path().join("loose.fabro");
    std::fs::write(
        &loose_workflow,
        "digraph Loose { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
    )
    .unwrap();

    let project_expected = fabro_manifest::resolve_local_workflow_package(
        std::path::Path::new("hello"),
        project.path(),
        Some(&user_root),
    )
    .unwrap();
    assert_eq!(
        project_expected.workflow_location().toml.as_ref(),
        Some(&project_workflow.canonicalize().unwrap())
    );
    let user_expected = fabro_manifest::resolve_local_workflow_package(
        std::path::Path::new("hello"),
        outside.path(),
        Some(&user_root),
    )
    .unwrap();
    assert_eq!(
        user_expected.workflow_location().toml.as_ref(),
        Some(&user_workflow.canonicalize().unwrap())
    );
    let other_expected = fabro_manifest::resolve_local_workflow_package(
        &other_workflow,
        outside.path(),
        Some(&user_root),
    )
    .unwrap();
    let loose_expected = fabro_manifest::resolve_local_workflow_package(
        &loose_workflow,
        outside.path(),
        Some(&user_root),
    )
    .unwrap();
    let expected = [
        project_expected.closure().root_id(),
        user_expected.closure().root_id(),
        other_expected.closure().root_id(),
        loose_expected.closure().root_id(),
    ];

    for (cwd, workflow) in [
        (project.path(), std::path::Path::new("hello")),
        (outside.path(), std::path::Path::new("hello")),
        (outside.path(), other_workflow.as_path()),
        (outside.path(), loose_workflow.as_path()),
    ] {
        let output = context
            .create_cmd()
            .current_dir(cwd)
            .args([
                "--server",
                &format!("{}/api/v1", server.base_url()),
                "--environment",
                "local",
                workflow.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "create failed for {}:\n{}",
            workflow.display(),
            output_stderr(&output)
        );
    }

    environment_mock.assert_calls(4);
    version_mock.assert_calls(4);
    create_mock.assert_calls(4);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), expected.len());
    for (request, expected_id) in requests.iter().zip(expected) {
        assert_eq!(request["workflow_version_id"], expected_id.to_string());
    }
    assert_eq!(
        requests[0]["target"]["path"],
        project.path().canonicalize().unwrap().to_str().unwrap()
    );
    for request in &requests[1..] {
        assert_eq!(
            request["target"]["path"],
            outside.path().canonicalize().unwrap().to_str().unwrap()
        );
    }
}

#[test]
fn create_clone_targets_require_exact_git_observations() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_calls = Arc::new(AtomicUsize::new(0));
    let environment_calls_for_mock = Arc::clone(&environment_calls);
    let environment_mock = server.mock(|when, then| {
        when.method("GET").path("/api/v1/environments/default");
        then.respond_with(move |_| {
            let provider = match environment_calls_for_mock.fetch_add(1, Ordering::SeqCst) {
                0 | 2 => "docker",
                1 => "daytona",
                call => panic!("unexpected environment retrieval {call}"),
            };
            HttpMockResponse::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(environment_json("default", provider).to_string())
                .build()
        });
    });
    let version_mock = mock_workflow_version_registrations(&server);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, Arc::clone(&requests));
    let fixture_root = tempfile::tempdir().unwrap();
    let workflow = write_workflow(fixture_root.path(), "workflow", "CloneTarget");

    let exact = tempfile::tempdir().unwrap();
    let bare = fixture_root.path().join("origin.git");
    run_git(fixture_root.path(), &[
        "init",
        "--bare",
        "--quiet",
        bare.to_str().unwrap(),
    ]);
    run_git(exact.path(), &[
        "-c",
        "init.defaultBranch=feature",
        "init",
        "--quiet",
    ]);
    std::fs::write(exact.path().join("tracked.txt"), "tracked").unwrap();
    run_git(exact.path(), &["add", "tracked.txt"]);
    run_git(exact.path(), &[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    run_git(exact.path(), &[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/widgets.git",
    ]);
    let local_url = format!("file://{}", bare.display());
    run_git(exact.path(), &[
        "remote", "set-url", "--push", "origin", &local_url,
    ]);
    std::fs::write(exact.path().join("dirty.txt"), "not committed").unwrap();
    let exact_output = context
        .create_cmd()
        .current_dir(exact.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--dry-run",
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        exact_output.status.success(),
        "{}",
        output_stderr(&exact_output)
    );
    assert_eq!(
        output_stderr(&exact_output)
            .lines()
            .filter(|line| line.contains("working tree is dirty"))
            .count(),
        1
    );

    let branch_only = tempfile::tempdir().unwrap();
    run_git(branch_only.path(), &[
        "-c",
        "init.defaultBranch=topic",
        "init",
        "--quiet",
    ]);
    std::fs::write(branch_only.path().join("tracked.txt"), "tracked").unwrap();
    run_git(branch_only.path(), &["add", "tracked.txt"]);
    run_git(branch_only.path(), &[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    run_git(branch_only.path(), &[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/missing.git",
    ]);
    let missing_url = format!("file://{}/missing.git", fixture_root.path().display());
    run_git(branch_only.path(), &[
        "remote",
        "set-url",
        "--push",
        "origin",
        &missing_url,
    ]);
    let branch_output = context
        .create_cmd()
        .current_dir(branch_only.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!branch_output.status.success());
    let branch_stderr = output_stderr(&branch_output);
    assert!(
        branch_stderr.contains(
            "the exact local Git commit could not be made available from the canonical GitHub origin"
        ),
        "{branch_stderr}"
    );
    assert!(!branch_stderr.contains("file://"));
    assert!(!branch_stderr.contains("No such file or directory"));

    let no_repository = tempfile::tempdir().unwrap();
    let none_output = context
        .create_cmd()
        .current_dir(no_repository.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        none_output.status.success(),
        "{}",
        output_stderr(&none_output)
    );

    environment_mock.assert_calls(3);
    version_mock.assert_calls(2);
    create_mock.assert_calls(2);
    let requests = requests.lock().unwrap();
    assert_eq!(requests[0]["args"]["dry_run"], true);
    assert_eq!(
        requests[0]["target"],
        json!({
            "kind": "git",
            "repo": "acme/widgets",
            "branch": "feature",
            "sha": run_git(exact.path(), &["rev-parse", "HEAD"]),
        })
    );
    assert_eq!(requests[1]["target"], json!({ "kind": "none" }));
}

#[test]
fn create_rejects_unusable_git_checkouts_instead_of_sending_an_empty_target() {
    let context = test_context!();
    let server = MockServer::start();
    let environment_mock = mock_environment(&server, "default", "docker");
    let version_mock = mock_workflow_version_registrations(&server);
    let run_id = unique_run_id();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, requests);
    let fixture_root = tempfile::tempdir().unwrap();
    let workflow = write_workflow(fixture_root.path(), "workflow", "UnusableCheckout");

    let detached = tempfile::tempdir().unwrap();
    run_git(detached.path(), &[
        "-c",
        "init.defaultBranch=feature",
        "init",
        "--quiet",
    ]);
    std::fs::write(detached.path().join("tracked.txt"), "tracked").unwrap();
    run_git(detached.path(), &["add", "tracked.txt"]);
    run_git(detached.path(), &[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    run_git(detached.path(), &["checkout", "--detach", "--quiet"]);

    let unborn = tempfile::tempdir().unwrap();
    run_git(unborn.path(), &[
        "-c",
        "init.defaultBranch=feature",
        "init",
        "--quiet",
    ]);

    for (working_directory, expected_error) in [
        (
            detached.path(),
            "the caller Git checkout has a detached HEAD",
        ),
        (unborn.path(), "the caller Git checkout has no commits"),
    ] {
        let output = context
            .create_cmd()
            .current_dir(working_directory)
            .args([
                "--server",
                &format!("{}/api/v1", server.base_url()),
                workflow.to_str().unwrap(),
            ])
            .output()
            .unwrap();

        assert!(!output.status.success());
        let stderr = output_stderr(&output);
        assert!(stderr.contains(expected_error), "{stderr}");
    }

    environment_mock.assert_calls(2);
    version_mock.assert_calls(0);
    create_mock.assert_calls(0);
}

#[test]
fn create_rejects_an_unsupported_attached_origin_before_upload() {
    let context = test_context!();
    let server = MockServer::start();
    let environment_mock = mock_environment(&server, "default", "docker");
    let version_mock = mock_workflow_version_registrations(&server);
    let run_id = unique_run_id();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, requests);
    let fixture_root = tempfile::tempdir().unwrap();
    let workflow = write_workflow(fixture_root.path(), "workflow", "UnsupportedOrigin");
    let workspace = tempfile::tempdir().unwrap();
    let bare = fixture_root.path().join("unsupported.git");
    run_git(fixture_root.path(), &[
        "init",
        "--bare",
        "--quiet",
        bare.to_str().unwrap(),
    ]);
    run_git(workspace.path(), &[
        "-c",
        "init.defaultBranch=feature",
        "init",
        "--quiet",
    ]);
    std::fs::write(workspace.path().join("tracked.txt"), "tracked").unwrap();
    run_git(workspace.path(), &["add", "tracked.txt"]);
    run_git(workspace.path(), &[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    run_git(workspace.path(), &[
        "remote",
        "add",
        "origin",
        bare.to_str().unwrap(),
    ]);

    let output = context
        .create_cmd()
        .current_dir(workspace.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        output_stderr(&output).contains("cannot be represented as a canonical GitHub run target")
    );
    environment_mock.assert();
    version_mock.assert_calls(0);
    create_mock.assert_calls(0);
}

#[test]
fn create_registers_dependencies_before_the_root_and_then_creates_once() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&server, "local", "local");
    let registrations = Arc::new(Mutex::new(Vec::new()));
    let version_mock =
        mock_workflow_version_registrations_recording(&server, Arc::clone(&registrations));
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let requests_for_mock = Arc::clone(&requests);
    let registrations_for_create = Arc::clone(&registrations);
    let create_response = run_status_response(run_id.as_str(), "submitted").to_string();
    let create_mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.respond_with(move |request| {
            assert_eq!(
                registrations_for_create.lock().unwrap().len(),
                2,
                "intent create arrived before dependency and root registration"
            );
            requests_for_mock
                .lock()
                .unwrap()
                .push(serde_json::from_slice(request.body_ref()).unwrap());
            HttpMockResponse::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(create_response.clone())
                .build()
        });
    });
    let project = tempfile::tempdir().unwrap();
    run_git(project.path(), &["init", "--quiet"]);
    write_workflow(project.path(), ".fabro/workflows/root", "Root");
    write_workflow(project.path(), ".fabro/workflows/child", "Child");
    std::fs::write(
        project.path().join(".fabro/workflows/root/workflow.fabro"),
        r#"digraph Root {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  child [shape=house, stack.child_workflow="../child/workflow.fabro"]
  start -> child -> exit
}"#,
    )
    .unwrap();
    let expected = fabro_manifest::resolve_local_workflow_package(
        std::path::Path::new("root"),
        project.path(),
        None,
    )
    .unwrap();

    let output = context
        .create_cmd()
        .current_dir(project.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--environment",
            "local",
            "root",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_stderr(&output));
    environment_mock.assert();
    version_mock.assert_calls(2);
    create_mock.assert_calls(1);
    let registered_entrypoints: Vec<String> = registrations
        .lock()
        .unwrap()
        .iter()
        .map(|version| version["entrypoint"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(registered_entrypoints, [
        ".fabro/workflows/child/workflow.fabro",
        ".fabro/workflows/root/workflow.fabro",
    ]);
    assert_eq!(
        requests.lock().unwrap()[0]["workflow_version_id"],
        expected.closure().root_id().to_string()
    );
}

#[test]
fn create_stops_before_intent_create_when_registration_fails() {
    let context = test_context!();
    let server = MockServer::start();
    let environment_mock = mock_environment(&server, "local", "local");
    let version_mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/workflow-versions");
        then.status(409)
            .header("content-type", "text/plain")
            .body("workflow version rejected by fixture");
    });
    let run_id = unique_run_id();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, requests);
    let source = tempfile::tempdir().unwrap();
    let caller = tempfile::tempdir().unwrap();
    let workflow = write_workflow(source.path(), "workflow", "RegistrationFailure");

    let output = context
        .create_cmd()
        .current_dir(caller.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--environment",
            "local",
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    environment_mock.assert();
    version_mock.assert();
    create_mock.assert_calls(0);
    let stderr = output_stderr(&output);
    assert!(
        stderr.contains("could not register workflow versions"),
        "{stderr}"
    );
    assert!(stderr.contains("index 0"), "{stderr}");
    assert!(
        stderr.contains("workflow version rejected by fixture"),
        "{stderr}"
    );
    assert!(stderr.contains("409 Conflict"), "{stderr}");
}

#[test]
fn create_persists_directory_workflow_slug_and_cached_graph() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let workflow_path = context.temp_dir.join("sluggy/workflow.fabro");

    context.write_temp(
        "sluggy/workflow.fabro",
        "\
digraph BarBaz {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    let create = context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--environment",
            "local",
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = created_run_id(create.get_output());

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    let run = &state.spec;
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": run.workflow_slug,
            "graph_name": run.graph.name,
            "cached_graph_lines": state.spec.graph_source.as_ref().expect("graph should exist").lines().collect::<Vec<_>>(),
        }),
        @r#"
        {
          "workflow_slug": "workflow",
          "graph_name": "BarBaz",
          "cached_graph_lines": [
            "digraph BarBaz {",
            "  start [shape=Mdiamond, label=\"Start\"]",
            "  exit  [shape=Msquare, label=\"Exit\"]",
            "  start -> exit",
            "}"
          ]
        }
        "#
    );
}

#[test]
fn create_persists_file_stem_slug_for_standalone_file() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let workflow_path = context.temp_dir.join("alpha.fabro");

    context.write_temp(
        "alpha.fabro",
        "\
digraph FooWorkflow {
  start [shape=Mdiamond, label=\"Start\"]
  exit  [shape=Msquare, label=\"Exit\"]
  start -> exit
}
",
    );

    let create = context
        .command()
        .args([
            "create",
            "--dry-run",
            "--auto-approve",
            "--environment",
            "local",
            workflow_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let run_id = created_run_id(create.get_output());

    let run_dir = context.find_run_dir(&run_id);
    let state = run_state(&run_dir);
    let run = &state.spec;
    fabro_json_snapshot!(
        context,
        serde_json::json!({
            "workflow_slug": run.workflow_slug,
            "graph_name": run.graph.name,
            "cached_graph_lines": state.spec.graph_source.as_ref().expect("graph should exist").lines().collect::<Vec<_>>(),
        }),
        @r#"
        {
          "workflow_slug": "alpha",
          "graph_name": "FooWorkflow",
          "cached_graph_lines": [
            "digraph FooWorkflow {",
            "  start [shape=Mdiamond, label=\"Start\"]",
            "  exit  [shape=Msquare, label=\"Exit\"]",
            "  start -> exit",
            "}"
          ]
        }
        "#
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "test asserts the raw template source"
)]
#[test]
fn create_persists_requested_overrides_into_store() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let workflow = fixture("simple.fabro");
    let mut cmd = context.command();
    cmd.args([
        "create",
        "--dry-run",
        "--auto-approve",
        "--goal",
        "Ship the release",
        "--model",
        "gpt-5",
        "--provider",
        "openai",
        "--environment",
        "local",
        "--label",
        "env=dev",
        "--label",
        "team=cli",
        "--verbose",
        "--preserve-sandbox",
        workflow.to_str().unwrap(),
    ]);
    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = output_stdout(&output);
    let run_id = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .expect("create should print a run ID")
        .to_string();
    let run = resolve_run(&context, &run_id);
    let state = run_state(&run.run_dir);
    let run_spec = &state.spec;
    let labels = json!({
        "env": run_spec.labels.get("env"),
        "team": run_spec.labels.get("team"),
    });
    let settings = &run_spec.settings;
    let resolved_run = resolved_run(settings);
    let compact = json!({
        "workflow_slug": run_spec.workflow_slug,
        "settings": {
            "goal": match resolved_run.goal.as_ref() {
                Some(fabro_types::settings::run::RunGoal::Inline(value)) => Some(value.as_source()),
                _ => None,
            },
            "dry_run": resolved_run.execution.mode == fabro_types::settings::run::RunMode::DryRun,
            "auto_approve": resolved_run.execution.approval == fabro_types::settings::run::ApprovalMode::Auto,
            "llm": {
                "model": resolved_run.model.name.clone(),
                "provider": resolved_run.model.provider.clone(),
            },
            "environment": {
                "id": resolved_run.environment.id,
                "provider": resolved_run.environment.provider.to_string(),
                "preserve": resolved_run.environment.lifecycle.preserve,
            },
        },
        "labels": labels,
    });

    assert_snapshot!(serde_json::to_string_pretty(&compact).unwrap(), @r###"
    {
      "workflow_slug": "simple",
      "settings": {
        "goal": "Ship the release",
        "dry_run": true,
        "auto_approve": true,
        "llm": {
          "model": "gpt-5",
          "provider": "openai"
        },
        "environment": {
          "id": "local",
          "provider": "local",
          "preserve": true
        }
      },
      "labels": {
        "env": "dev",
        "team": "cli"
      }
    }
    "###);
}

#[test]
fn create_json_does_not_imply_auto_approve() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let workflow = fixture("simple.fabro");
    let output = context
        .command()
        .args([
            "--json",
            "create",
            "--dry-run",
            "--environment",
            "local",
            workflow.to_str().unwrap(),
        ])
        .output()
        .expect("command should execute");

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("create JSON should parse");
    let run_id = value["run_id"]
        .as_str()
        .expect("create JSON should include run_id");
    let run = resolve_run(&context, run_id);

    assert!(
        resolved_run(&run_state(&run.run_dir).spec.settings,)
            .execution
            .approval
            != fabro_types::settings::run::ApprovalMode::Auto
    );
}

#[test]
fn create_invalid_workflow_fails_without_creating_run() {
    let context = test_context!();
    let caller = tempfile::tempdir().unwrap();
    let workflow = fixture("invalid.fabro");
    let initial_run_count = run_count_for_test_case(&context);
    let mut cmd = context.create_cmd();
    cmd.current_dir(caller.path())
        .args(["--quiet", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × could not create run
      ╰─▶ run intent could not be compiled: Validation failed
    ");

    let run_count = run_count_for_test_case(&context);
    assert_eq!(
        run_count, initial_run_count,
        "invalid create should not persist a run for this test case"
    );
}

#[test]
fn create_rejects_unbound_template_inputs_without_creating_run() {
    let context = test_context!();
    let caller = tempfile::tempdir().unwrap();
    let workflow = fixture("templated_unbound.fabro");
    let initial_run_count = run_count_for_test_case(&context);
    let mut cmd = context.create_cmd();
    cmd.current_dir(caller.path())
        .args(["--quiet", workflow.to_str().unwrap()]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × could not create run
      ╰─▶ run intent could not be compiled: Validation failed
    ");

    let run_count = run_count_for_test_case(&context);
    assert_eq!(
        run_count, initial_run_count,
        "invalid create should not persist a run for this test case"
    );
}

#[test]
fn create_registers_package_before_surfacing_server_admission_rejection() {
    let context = test_context!();
    let server = MockServer::start();
    let environment_mock = mock_environment(&server, "local", "local");
    let registered_versions = Arc::new(Mutex::new(Vec::new()));
    let version_mock =
        mock_workflow_version_registrations_recording(&server, Arc::clone(&registered_versions));
    let registered_versions_for_create = Arc::clone(&registered_versions);
    let create_mock = server.mock(|when, then| {
        when.method("POST").path("/api/v1/runs");
        then.respond_with(move |_| {
            assert_eq!(
                registered_versions_for_create.lock().unwrap().len(),
                1,
                "the workflow version must be registered before server admission"
            );
            HttpMockResponse::builder()
                .status(422)
                .header("content-type", "text/plain")
                .body("server-authoritative workflow rejection")
                .build()
        });
    });
    let output = context
        .create_cmd()
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--environment",
            "local",
            fixture("templated_unbound.fabro").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    environment_mock.assert();
    version_mock.assert();
    create_mock.assert();
    let stderr = output_stderr(&output);
    assert!(stderr.contains("could not create run"), "{stderr}");
    assert!(
        stderr.contains("server-authoritative workflow rejection"),
        "{stderr}"
    );
    assert!(!stderr.contains("Validation failed"), "{stderr}");
}

#[test]
fn create_does_not_warn_for_client_only_settings() {
    let context = test_context!();
    let server = MockServer::start();
    let run_id = unique_run_id();
    let environment_mock = mock_environment(&server, "local", "local");
    let version_mock = mock_workflow_version_registrations(&server);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let create_mock = mock_intent_create(&server, &run_id, requests);
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".fabro")).unwrap();
    std::fs::write(
        project.path().join(".fabro/project.toml"),
        "_version = 1\n\n[cli.output]\nverbosity = \"quiet\"\n",
    )
    .unwrap();
    let workflow = write_workflow(project.path(), "workflow", "ClientOnly");
    context.write_home(
        ".fabro/settings.toml",
        "_version = 1\n\n[cli.output]\nverbosity = \"quiet\"\n",
    );

    let output = context
        .create_cmd()
        .current_dir(project.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            "--environment",
            "local",
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_stderr(&output));
    environment_mock.assert();
    version_mock.assert();
    create_mock.assert();
    assert!(!output_stderr(&output).contains("do not transmit these settings"));
}

#[test]
fn create_rejects_malformed_discovered_project_config_before_server_access() {
    let context = test_context!();
    let server = MockServer::start();
    let any_request = server.mock(|when, then| {
        when.any_request();
        then.status(500).body("server must remain untouched");
    });
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".fabro")).unwrap();
    let project_config = project.path().join(".fabro/project.toml");
    std::fs::write(&project_config, "_version = 1\nrun = [").unwrap();
    let workflow = write_workflow(project.path(), "workflow", "MalformedProject");

    let output = context
        .create_cmd()
        .current_dir(project.path())
        .args([
            "--server",
            &format!("{}/api/v1", server.base_url()),
            workflow.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = output_stderr(&output);
    assert!(
        stderr.contains(project_config.to_str().unwrap()),
        "{stderr}"
    );
    assert!(
        stderr.contains("TOML") || stderr.contains("toml"),
        "{stderr}"
    );
    any_request.assert_calls(0);
}

#[test]
fn create_uses_workflow_owned_pull_request_settings_only() {
    let context = test_context!();
    context.ensure_home_server_auth_methods();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".fabro")).unwrap();
    std::fs::write(
        project.path().join(".fabro/project.toml"),
        r"_version = 1

[run.pull_request]
enabled = true
draft = false
",
    )
    .unwrap();
    let workflow = write_workflow(project.path(), "workflow", "PullRequestAuthority");

    let project_only = context
        .create_cmd()
        .current_dir(project.path())
        .args(["--environment", "local", workflow.to_str().unwrap()])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&project_only.get_output().stderr)
            .contains("project.toml contains run")
    );
    let project_only_id = created_run_id(project_only.get_output());
    let project_only_state = run_state(&context.find_run_dir(&project_only_id));
    assert_eq!(
        project_only_state.spec.settings.run.pull_request, None,
        "project-only pull-request settings must not cross intent admission"
    );

    std::fs::write(
        &workflow,
        r#"_version = 1

[workflow]
graph = "workflow.fabro"

[run.pull_request]
enabled = true
draft = false
"#,
    )
    .unwrap();
    let workflow_owned = context
        .create_cmd()
        .current_dir(project.path())
        .args(["--environment", "local", workflow.to_str().unwrap()])
        .assert()
        .success();
    let workflow_owned_id = created_run_id(workflow_owned.get_output());
    let workflow_owned_state = run_state(&context.find_run_dir(&workflow_owned_id));
    let pull_request = workflow_owned_state
        .spec
        .settings
        .run
        .pull_request
        .as_ref()
        .expect("workflow.toml should configure pull-request behavior");
    assert!(pull_request.enabled);
    assert!(!pull_request.draft);
}
