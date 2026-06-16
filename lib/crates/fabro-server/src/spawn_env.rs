use std::ffi::OsString;

use fabro_static::EnvVars;
use tokio::process::Command;

const WORKER_ENV_ALLOWLIST: &[&str] = &[
    EnvVars::PATH,
    EnvVars::HOME,
    EnvVars::TMPDIR,
    EnvVars::USER,
    EnvVars::RUST_LOG,
    EnvVars::RUST_BACKTRACE,
    EnvVars::FABRO_LOG,
    EnvVars::FABRO_HOME,
    EnvVars::FABRO_STORAGE_ROOT,
    EnvVars::TERM,
    EnvVars::NO_COLOR,
    EnvVars::CLICOLOR,
    EnvVars::CLICOLOR_FORCE,
    // AWS credential-chain inputs for the Bedrock provider. Other providers'
    // secrets reach the worker through the server vault (read via FABRO_HOME),
    // but Bedrock SigV4 has no stored secret — it re-resolves from the ambient
    // AWS chain on every request so STS/SSO/IRSA sessions can refresh, which
    // means the chain's *inputs* must survive `env_clear()` in the worker, not
    // a snapshot taken at launch. We pass the identity surface only (static
    // keys, session token, profile/region selectors, and the web-identity/ECS
    // role vars); HOME already carries the shared
    // `~/.aws` config + SSO cache. Endpoint/metadata overrides
    // (AWS_ENDPOINT_*, AWS_METADATA_ENDPOINT, AWS_IMDSV1_FALLBACK) are
    // deliberately excluded — they belong to the server's S3 path, not to the
    // worker's outbound model calls. Bedrock bearer API keys are optional LLM
    // provider secrets, so server workers read them through the vault rather
    // than inheriting process env.
    EnvVars::AWS_ACCESS_KEY_ID,
    EnvVars::AWS_SECRET_ACCESS_KEY,
    EnvVars::AWS_SESSION_TOKEN,
    EnvVars::AWS_PROFILE,
    EnvVars::AWS_REGION,
    EnvVars::AWS_DEFAULT_REGION,
    EnvVars::AWS_ROLE_ARN,
    EnvVars::AWS_ROLE_SESSION_NAME,
    EnvVars::AWS_WEB_IDENTITY_TOKEN_FILE,
    EnvVars::AWS_CONTAINER_CREDENTIALS_RELATIVE_URI,
    EnvVars::AWS_CONTAINER_CREDENTIALS_FULL_URI,
    EnvVars::AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE,
];

const RENDER_GRAPH_ENV_ALLOWLIST: &[&str] = &[EnvVars::PATH, EnvVars::HOME, EnvVars::TMPDIR];

pub(crate) fn apply_worker_env(cmd: &mut Command) {
    apply_allowlist(cmd, WORKER_ENV_ALLOWLIST, &process_env_var_os);
}

pub(crate) fn apply_render_graph_env(cmd: &mut Command) {
    apply_allowlist(cmd, RENDER_GRAPH_ENV_ALLOWLIST, &process_env_var_os);
}

#[expect(
    clippy::disallowed_methods,
    reason = "Subprocess env allowlists intentionally copy a narrow process-env subset."
)]
fn process_env_var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

fn apply_allowlist(cmd: &mut Command, keys: &[&str], lookup: &dyn Fn(&str) -> Option<OsString>) {
    cmd.env_clear();
    for key in keys {
        if let Some(value) = lookup(key) {
            cmd.env(key, value);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::Path;

    use super::{RENDER_GRAPH_ENV_ALLOWLIST, WORKER_ENV_ALLOWLIST, apply_allowlist};

    fn env_command() -> tokio::process::Command {
        assert!(Path::new("/usr/bin/env").exists());
        tokio::process::Command::new("/usr/bin/env")
    }

    async fn env_output(mut cmd: tokio::process::Command) -> HashMap<String, String> {
        let output = cmd.output().await.expect("running env subprocess");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("parsing env subprocess output as UTF-8")
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect()
    }

    #[tokio::test]
    async fn worker_allowlist_is_fail_closed() {
        let env = HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("HOME".to_string(), "/tmp/home".to_string()),
            ("TMPDIR".to_string(), "/tmp".to_string()),
            ("USER".to_string(), "alice".to_string()),
            ("RUST_LOG".to_string(), "debug".to_string()),
            ("FABRO_LOG".to_string(), "debug".to_string()),
            ("FABRO_LOG_DESTINATION".to_string(), "stdout".to_string()),
            ("FABRO_HOME".to_string(), "/tmp/fabro-home".to_string()),
            (
                "FABRO_STORAGE_ROOT".to_string(),
                "/tmp/fabro-storage".to_string(),
            ),
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("NO_COLOR".to_string(), "1".to_string()),
            ("CLICOLOR".to_string(), "0".to_string()),
            ("CLICOLOR_FORCE".to_string(), "1".to_string()),
            ("AWS_ACCESS_KEY_ID".to_string(), "AKIAEXAMPLE".to_string()),
            ("AWS_SECRET_ACCESS_KEY".to_string(), "secret".to_string()),
            ("AWS_SESSION_TOKEN".to_string(), "session".to_string()),
            ("AWS_BEARER_TOKEN_BEDROCK".to_string(), "bearer".to_string()),
            ("BEDROCK_API_KEY".to_string(), "alias-bearer".to_string()),
            ("AWS_REGION".to_string(), "us-east-2".to_string()),
            ("SESSION_SECRET".to_string(), "leak".to_string()),
            ("FABRO_JWT_PRIVATE_KEY".to_string(), "leak".to_string()),
            ("FABRO_JWT_PUBLIC_KEY".to_string(), "leak".to_string()),
            ("GITHUB_APP_PRIVATE_KEY".to_string(), "leak".to_string()),
            ("GITHUB_APP_CLIENT_SECRET".to_string(), "leak".to_string()),
            ("GITHUB_APP_WEBHOOK_SECRET".to_string(), "leak".to_string()),
            ("FABRO_DEV_TOKEN".to_string(), "garbage".to_string()),
            ("FABRO_WORKER_TOKEN".to_string(), "leak".to_string()),
            ("MY_API_KEY".to_string(), "blocked".to_string()),
        ]);
        let mut cmd = env_command();
        apply_allowlist(&mut cmd, WORKER_ENV_ALLOWLIST, &|name| {
            env.get(name).map(OsString::from)
        });
        cmd.env(
            "FABRO_DEV_TOKEN",
            "fabro_dev_abababababababababababababababababababababababababababababababab",
        );

        let actual = env_output(cmd).await;

        assert_eq!(actual.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(actual.get("HOME").map(String::as_str), Some("/tmp/home"));
        assert_eq!(actual.get("FABRO_LOG").map(String::as_str), Some("debug"));
        assert_eq!(
            actual.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
        assert_eq!(actual.get("NO_COLOR").map(String::as_str), Some("1"));
        assert_eq!(actual.get("CLICOLOR").map(String::as_str), Some("0"));
        assert_eq!(actual.get("CLICOLOR_FORCE").map(String::as_str), Some("1"));
        // Bedrock SigV4 chain inputs cross into the worker so it can re-resolve
        // credentials per request; a generic secret with no allowlist entry
        // still does not.
        assert_eq!(
            actual.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("AKIAEXAMPLE")
        );
        assert_eq!(
            actual.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("secret")
        );
        assert_eq!(
            actual.get("AWS_SESSION_TOKEN").map(String::as_str),
            Some("session")
        );
        assert_eq!(
            actual.get("AWS_REGION").map(String::as_str),
            Some("us-east-2")
        );
        assert!(!actual.contains_key("AWS_BEARER_TOKEN_BEDROCK"));
        assert!(!actual.contains_key("BEDROCK_API_KEY"));
        assert!(!actual.contains_key("FABRO_LOG_DESTINATION"));
        assert_eq!(
            actual.get("FABRO_DEV_TOKEN").map(String::as_str),
            Some("fabro_dev_abababababababababababababababababababababababababababababababab")
        );
        assert!(!actual.contains_key("SESSION_SECRET"));
        assert!(!actual.contains_key("FABRO_JWT_PRIVATE_KEY"));
        assert!(!actual.contains_key("FABRO_JWT_PUBLIC_KEY"));
        assert!(!actual.contains_key("GITHUB_APP_PRIVATE_KEY"));
        assert!(!actual.contains_key("GITHUB_APP_CLIENT_SECRET"));
        assert!(!actual.contains_key("GITHUB_APP_WEBHOOK_SECRET"));
        assert!(!actual.contains_key("FABRO_WORKER_TOKEN"));
        assert!(!actual.contains_key("MY_API_KEY"));
    }

    #[tokio::test]
    async fn render_graph_allowlist_is_fail_closed() {
        let env = HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("HOME".to_string(), "/tmp/home".to_string()),
            ("TMPDIR".to_string(), "/tmp".to_string()),
            ("FABRO_TELEMETRY".to_string(), "on".to_string()),
            ("SESSION_SECRET".to_string(), "leak".to_string()),
        ]);
        let mut cmd = env_command();
        apply_allowlist(&mut cmd, RENDER_GRAPH_ENV_ALLOWLIST, &|name| {
            env.get(name).map(OsString::from)
        });
        cmd.env("FABRO_TELEMETRY", "off");

        let actual = env_output(cmd).await;

        assert_eq!(actual.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            actual.get("FABRO_TELEMETRY").map(String::as_str),
            Some("off")
        );
        assert!(!actual.contains_key("SESSION_SECRET"));
    }
}
