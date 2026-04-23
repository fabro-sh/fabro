use std::ffi::OsString;

use tokio::process::Command;

const WORKER_ENV_ALLOWLIST: &[&str] = &[
    "PATH",               // process essentials
    "HOME",               // process essentials
    "TMPDIR",             // temp file staging
    "USER",               // process identity
    "RUST_LOG",           // diagnostics
    "RUST_BACKTRACE",     // diagnostics
    "FABRO_HOME",         // worker state lookup
    "FABRO_STORAGE_ROOT", // worker state lookup
];

const RENDER_GRAPH_ENV_ALLOWLIST: &[&str] = &[
    "PATH",   // executable lookup
    "HOME",   // graphviz/font resolution
    "TMPDIR", // temp file staging
];

pub(crate) fn apply_worker_env(cmd: &mut Command) {
    apply_worker_env_with_lookup(cmd, &|name| std::env::var_os(name));
}

pub(crate) fn apply_render_graph_env(cmd: &mut Command) {
    apply_render_graph_env_with_lookup(cmd, &|name| std::env::var_os(name));
}

fn apply_worker_env_with_lookup(cmd: &mut Command, lookup: &dyn Fn(&str) -> Option<OsString>) {
    apply_allowlist(cmd, WORKER_ENV_ALLOWLIST, lookup);
}

fn apply_render_graph_env_with_lookup(
    cmd: &mut Command,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) {
    apply_allowlist(cmd, RENDER_GRAPH_ENV_ALLOWLIST, lookup);
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

    use super::{apply_render_graph_env_with_lookup, apply_worker_env_with_lookup};

    fn env_command() -> tokio::process::Command {
        assert!(Path::new("/usr/bin/env").exists());
        tokio::process::Command::new("/usr/bin/env")
    }

    fn env_output(mut cmd: tokio::process::Command) -> HashMap<String, String> {
        let runtime = tokio::runtime::Runtime::new().expect("creating test Tokio runtime");
        runtime.block_on(async move {
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
        })
    }

    #[test]
    fn worker_allowlist_is_fail_closed() {
        let env = HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("HOME".to_string(), "/tmp/home".to_string()),
            ("TMPDIR".to_string(), "/tmp".to_string()),
            ("USER".to_string(), "alice".to_string()),
            ("RUST_LOG".to_string(), "debug".to_string()),
            ("FABRO_HOME".to_string(), "/tmp/fabro-home".to_string()),
            (
                "FABRO_STORAGE_ROOT".to_string(),
                "/tmp/fabro-storage".to_string(),
            ),
            ("SESSION_SECRET".to_string(), "leak".to_string()),
            ("FABRO_DEV_TOKEN".to_string(), "garbage".to_string()),
            ("MY_API_KEY".to_string(), "blocked".to_string()),
        ]);
        let mut cmd = env_command();
        apply_worker_env_with_lookup(&mut cmd, &|name| env.get(name).map(OsString::from));
        cmd.env(
            "FABRO_DEV_TOKEN",
            "fabro_dev_abababababababababababababababababababababababababababababababab",
        );

        let actual = env_output(cmd);

        assert_eq!(actual.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(actual.get("HOME").map(String::as_str), Some("/tmp/home"));
        assert_eq!(
            actual.get("FABRO_DEV_TOKEN").map(String::as_str),
            Some("fabro_dev_abababababababababababababababababababababababababababababababab")
        );
        assert!(!actual.contains_key("SESSION_SECRET"));
        assert!(!actual.contains_key("MY_API_KEY"));
    }

    #[test]
    fn render_graph_allowlist_is_fail_closed() {
        let env = HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("HOME".to_string(), "/tmp/home".to_string()),
            ("TMPDIR".to_string(), "/tmp".to_string()),
            ("FABRO_TELEMETRY".to_string(), "on".to_string()),
            ("SESSION_SECRET".to_string(), "leak".to_string()),
        ]);
        let mut cmd = env_command();
        apply_render_graph_env_with_lookup(&mut cmd, &|name| env.get(name).map(OsString::from));
        cmd.env("FABRO_TELEMETRY", "off");

        let actual = env_output(cmd);

        assert_eq!(actual.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            actual.get("FABRO_TELEMETRY").map(String::as_str),
            Some("off")
        );
        assert!(!actual.contains_key("SESSION_SECRET"));
    }
}
