mod auth_harness;
mod auth_tokens;

use assert_cmd::Command;
pub(crate) use auth_harness::{
    RealAuthHarness, TEST_DEV_TOKEN, complete_login_via_browser, expire_saved_access_token,
    no_redirect_browser_client, run_detached, saved_auth_entry, seed_dev_token_auth,
};
pub(crate) use auth_tokens::{TEST_SESSION_SECRET, issue_test_github_jwt};
use fabro_store::EventEnvelope;
use fabro_test::{EnvVars, TestContext, preserve_coverage_env};
use fabro_types::RunId;

pub(crate) fn run_output_filters(context: &TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b\d+ms\b".to_string(), "[TIME]".to_string()));
    filters.push((
        r"(?m)^(Graph: ).+$".to_string(),
        "${1}[GRAPH_PATH]".to_string(),
    ));
    filters
}

pub(crate) fn fatal_error_line(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    console::strip_ansi_codes(&stderr)
        .lines()
        .rev()
        .find_map(|line| {
            line.strip_prefix("error: ")
                .or_else(|| line.trim_start().strip_prefix("× "))
                .map(ToOwned::to_owned)
        })
        .expect("stderr should contain a fatal error line")
}

pub(crate) fn unique_run_id() -> String {
    RunId::new().to_string()
}

pub(crate) fn parse_event_envelopes(response: &serde_json::Value) -> Vec<EventEnvelope> {
    response["data"]
        .as_array()
        .expect("event list response should contain a data array")
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .expect("wire event envelope list should parse")
}

pub(crate) struct LightweightCli {
    home_dir: tempfile::TempDir,
}

impl LightweightCli {
    pub(crate) fn new() -> Self {
        Self {
            home_dir: tempfile::tempdir().expect("temp home dir should exist"),
        }
    }

    pub(crate) fn home(&self) -> &std::path::Path {
        self.home_dir.path()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "Lightweight CLI test harness reconstructs a minimal process env for subprocesses."
    )]
    pub(crate) fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
        cmd.env_clear();
        preserve_coverage_env!(cmd);
        if let Some(path) = std::env::var_os(EnvVars::PATH) {
            cmd.env(EnvVars::PATH, path);
        }
        cmd.env(EnvVars::HOME, self.home_dir.path());
        cmd.env(EnvVars::NO_COLOR, "1");
        cmd.env(EnvVars::FABRO_NO_UPGRADE_CHECK, "true")
            .env(EnvVars::FABRO_HTTP_PROXY_POLICY, "disabled");
        cmd.current_dir(self.home_dir.path());
        cmd
    }
}
