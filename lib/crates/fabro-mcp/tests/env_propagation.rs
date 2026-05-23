//! Black-box tests that demonstrate the env-propagation gaps in
//! `scotts_docs/issues/fabro-mcp-env-propagation.md`.
//!
//! Each test takes a TOML snippet that exercises one documented way of getting
//! an env var to an MCP, resolves it through the same public API the CLI uses
//! (`WorkflowSettingsBuilder::from_toml`), spawns the existing Python test MCP
//! at `tests/test_mcp_server.py`, and asks the subprocess (via the
//! `__env:KEY__` echo sentinel) what it actually sees in `os.environ`. The
//! assertions encode the *user-facing expectation*, so each test fails today
//! and will pass once the underlying bug is fixed.
//!
//! The MCP key name (`TARGET`) is intentionally distinct from any
//! `{{ env.SOURCE_VAR }}` source key — `McpClient` runs with `clear_env=false`
//! so the child inherits the parent's env, and a same-named source key would
//! make the test pass by accident via inheritance instead of through the code
//! path under test.

use std::time::Duration;

use fabro_config::WorkflowSettingsBuilder;
use fabro_mcp::client::McpClient;
use fabro_mcp::connection_manager::call_result_to_string;
use fabro_types::settings::run::McpTransport;
use fabro_vault::{SecretType, Vault};

fn test_server_path() -> String {
    format!("{}/tests/test_mcp_server.py", env!("CARGO_MANIFEST_DIR"))
}

fn settings_toml(server_path: &str, extra: &str) -> String {
    format!(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[run.agent.mcps.envcheck]
type = "stdio"
command = ["python3", "{server_path}"]

{extra}
"#
    )
}

async fn mcp_observed_env(toml: &str, env_key: &str) -> String {
    let settings =
        WorkflowSettingsBuilder::from_toml(toml).expect("workflow settings should resolve");
    let mcp = settings
        .run
        .agent
        .mcps
        .get("envcheck")
        .expect("envcheck mcp should be present in resolved settings")
        .clone();
    let client = McpClient::new(&mcp).expect("McpClient::new should succeed");
    client
        .initialize(mcp.startup_timeout())
        .await
        .expect("MCP initialize");
    let result = client
        .call_tool(
            "echo",
            serde_json::json!({ "message": format!("__env:{env_key}__") }),
            Duration::from_secs(5),
        )
        .await
        .expect("echo tool call");
    let observed = call_result_to_string(&result).expect("echo result string");
    client.shutdown().await.expect("MCP shutdown");
    observed
}

/// Baseline / working path: `[run.agent.mcps.*.env]` with a *literal* value
/// (no template) reaches the MCP subprocess today. This test should PASS — it
/// documents the only currently-working way to pass an env var to an MCP from
/// `project.toml`. Note that the value is in plaintext in the TOML, which
/// makes this unsuitable for secrets.
#[tokio::test]
async fn mcp_env_table_with_literal_value_propagates_to_subprocess() {
    let expected = "baseline-literal-value";

    let toml = settings_toml(
        &test_server_path(),
        &format!(
            r#"[run.agent.mcps.envcheck.env]
TARGET = "{expected}"
"#
        ),
    );

    let observed = mcp_observed_env(&toml, "TARGET").await;

    assert_eq!(
        observed, expected,
        "literal values in [run.agent.mcps.envcheck.env] should reach the MCP \
         subprocess via `cmd.envs(env)` in fabro_mcp::client. If this test \
         fails, the baseline working path is broken.",
    );
}

/// Finding 1: `[run.agent.mcps.*.env]` with `{{ env.X }}` should resolve to
/// the parent process's value of `X` before the MCP child is spawned. Today
/// `fabro_config::resolve::run::resolve_mcp_entry` collapses the value via
/// `InterpString::as_source()`, so the literal template string is what reaches
/// the subprocess.
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "this test verifies env-variable interpolation end-to-end; the source var must \
              live in the parent process env. The name is unique to this test to avoid \
              clashing with parallel tests in the same binary."
)]
async fn mcp_env_table_with_env_template_resolves_in_subprocess() {
    let source_var = "FABRO_MCP_ENV_PROP_F1_SOURCE";
    let expected = "finding-1-secret";
    std::env::set_var(source_var, expected);

    let toml = settings_toml(
        &test_server_path(),
        &format!(
            r#"[run.agent.mcps.envcheck.env]
TARGET = "{{{{ env.{source_var} }}}}"
"#
        ),
    );

    let observed = mcp_observed_env(&toml, "TARGET").await;
    std::env::remove_var(source_var);

    assert_eq!(
        observed, expected,
        "MCP subprocess saw TARGET={observed:?} but should see {expected:?}; \
         [run.agent.mcps.envcheck.env] `{{{{ env.X }}}}` is not being resolved \
         (Finding 1 in scotts_docs/issues/fabro-mcp-env-propagation.md)",
    );
}

/// Finding 2 (literal value): `[run.sandbox.env]` is documented in
/// `docs/public/execution/run-configuration.mdx` as the canonical way to set
/// run-wide env. With no MCP-local `env` table at all, the MCP subprocess
/// should still see the sandbox env. Today it sees nothing because
/// `lib/crates/fabro-workflow/src/operations/start.rs:runtime_mcp_server`
/// never merges `SandboxEnvSpec.toml_env` into the MCP transport.
#[tokio::test]
async fn sandbox_env_with_literal_value_propagates_to_mcp_subprocess() {
    let expected = "finding-2-literal";

    let toml = settings_toml(
        &test_server_path(),
        &format!(
            r#"[run.sandbox.env]
TARGET = "{expected}"
"#
        ),
    );

    let observed = mcp_observed_env(&toml, "TARGET").await;

    assert_eq!(
        observed, expected,
        "MCP subprocess saw TARGET={observed:?} but should see {expected:?}; \
         [run.sandbox.env] does not propagate to MCP children \
         (Finding 2 in scotts_docs/issues/fabro-mcp-env-propagation.md)",
    );
}

/// Finding 2 (interpolated): same as the previous test but the sandbox env
/// value is itself a `{{ env.X }}` template. `[run.sandbox.env]` keeps
/// `InterpString` to runtime, so the template is preserved through resolve;
/// the gap is purely that the resolved value never reaches the MCP child.
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test sets a unique parent env var to drive {{ env.X }} interpolation; see \
              the rationale on mcp_env_table_with_env_template_resolves_in_subprocess."
)]
async fn sandbox_env_with_env_template_propagates_to_mcp_subprocess() {
    let source_var = "FABRO_MCP_ENV_PROP_F2_SOURCE";
    let expected = "finding-2-interpolated";
    std::env::set_var(source_var, expected);

    let toml = settings_toml(
        &test_server_path(),
        &format!(
            r#"[run.sandbox.env]
TARGET = "{{{{ env.{source_var} }}}}"
"#
        ),
    );

    let observed = mcp_observed_env(&toml, "TARGET").await;
    std::env::remove_var(source_var);

    assert_eq!(
        observed, expected,
        "MCP subprocess saw TARGET={observed:?} but should see {expected:?}; \
         [run.sandbox.env] `{{{{ env.X }}}}` is not being resolved into MCP children \
         (Finding 2 in scotts_docs/issues/fabro-mcp-env-propagation.md)",
    );
}

/// Finding 1 — sandbox transport variant. `type = "sandbox"` MCPs run inside
/// the workflow's Docker/Daytona container; their `env` is passed via
/// `sandbox.exec_command(..., env_ref, ...)` and ultimately `docker exec -e`.
/// The same `as_source()` bug in
/// `fabro_config::resolve::run::resolve_mcp_entry` (the `Sandbox` arm at
/// `lib/crates/fabro-config/src/resolve/run.rs:392-395`) means templates are
/// not resolved for sandbox MCPs either.
///
/// This test stops at the config layer rather than spawning a real container
/// — it asserts the resolved `McpTransport::Sandbox.env` contains the
/// resolved value, which is what `sandbox.exec_command` would forward.
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "same rationale as the stdio template tests above"
)]
async fn sandbox_mcp_env_table_with_env_template_resolves_at_config_layer() {
    let source_var = "FABRO_MCP_ENV_PROP_SANDBOX_F1_SOURCE";
    let expected = "sandbox-finding-1-secret";
    std::env::set_var(source_var, expected);

    let toml = format!(
        r#"
_version = 1

[server.auth]
methods = ["dev-token"]

[run.agent.mcps.envcheck]
type    = "sandbox"
command = ["python3", "/unused/in/this/test.py"]
port    = 3100

[run.agent.mcps.envcheck.env]
TARGET = "{{{{ env.{source_var} }}}}"
"#
    );

    let settings =
        WorkflowSettingsBuilder::from_toml(&toml).expect("workflow settings should resolve");
    let mcp = settings
        .run
        .agent
        .mcps
        .get("envcheck")
        .expect("envcheck mcp should be present");
    let resolved = match &mcp.transport {
        McpTransport::Sandbox { env, .. } => env.get("TARGET").cloned().unwrap_or_default(),
        other => panic!("expected Sandbox transport, got {other:?}"),
    };
    std::env::remove_var(source_var);

    assert_eq!(
        resolved, expected,
        "Sandbox McpTransport.env contained TARGET={resolved:?} but should hold {expected:?}; \
         [run.agent.mcps.X.env] `{{{{ env.X }}}}` is not being resolved for sandbox transport \
         either (Finding 1 variant in scotts_docs/issues/fabro-mcp-env-propagation.md)",
    );
}

/// Finding 3: `fabro secret set --type environment TARGET hunter2` puts the
/// secret in the vault (the only persistent place it lives), but no code path
/// reads the vault when launching an MCP child. The user-facing expectation
/// — secrets of type=environment are available to MCPs without further
/// configuration — is encoded as this assertion. Today: fails with TARGET="".
#[tokio::test]
async fn fabro_secret_environment_type_propagates_to_mcp_subprocess() {
    let expected = "finding-3-secret";

    // Mirror what `fabro secret set TARGET <value> --type environment` does
    // internally: load the vault and call `Vault::set`. This is the same
    // SecretType::Environment storage path the CLI uses.
    let vault_dir = tempfile::tempdir().expect("vault tempdir");
    let mut vault = Vault::load(vault_dir.path().join("secrets.json")).expect("vault should load");
    vault
        .set("TARGET", expected, SecretType::Environment, None)
        .expect("vault set should succeed");
    assert_eq!(
        vault.get("TARGET"),
        Some(expected),
        "vault should hold the secret we just stored",
    );

    // Plain MCP config with NO `env` table and NO `[run.sandbox.env]` — the
    // user relies entirely on `fabro secret set` to get the value to the MCP.
    let toml = settings_toml(&test_server_path(), "");
    let observed = mcp_observed_env(&toml, "TARGET").await;

    assert_eq!(
        observed,
        expected,
        "MCP subprocess saw TARGET={observed:?} but should see {expected:?}; \
         secrets of SecretType::Environment do not auto-inject into MCP child env \
         (Finding 3 in scotts_docs/issues/fabro-mcp-env-propagation.md). \
         The vault contains TARGET={:?}, but nothing in the MCP launch path consults it.",
        vault.get("TARGET"),
    );
}
