# CLI GitLab Install Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `fabro install gitlab` so operators can reconfigure GitLab token or GitLab OAuth-app integration from the CLI, including self-hosted GitLab base URLs.

**Architecture:** Mirror the existing `fabro install github` subcommand shape, but reuse GitLab's existing server-install semantics: GitLab token mode stores `GITLAB_TOKEN`, and GitLab app mode stores `GITLAB_TOKEN` plus `GITLAB_APP_CLIENT_SECRET`. Keep GitLab App semantics honest: OAuth is for human login; repository automation still uses the configured GitLab token.

**Tech Stack:** Rust, clap, tokio, reqwest via `fabro_http`, `fabro_gitlab`, `fabro_install`, `fabro_vault`, `insta`, `cargo nextest`.

---

## File Structure

- Modify `lib/crates/fabro-cli/src/args.rs`: add GitLab strategy and subcommand arguments.
- Modify `lib/crates/fabro-cli/src/main.rs`: add clap parser unit coverage for the new command.
- Modify `lib/crates/fabro-cli/src/commands/install.rs`: implement GitLab selection, validation, settings/vault persistence, and restart behavior.
- Modify `lib/crates/fabro-cli/tests/it/cmd/install.rs`: add public CLI contract tests and snapshots.
- Modify `docs/public/administration/server-configuration.mdx`: document the CLI command beside existing GitLab config docs.
- Read-only reference `docs/internal/testing-strategy.md`: already required for CLI integration test placement.

## Task 1: Add CLI Argument Surface

**Files:**
- Modify: `lib/crates/fabro-cli/src/args.rs`
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Test: `lib/crates/fabro-cli/src/main.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/install.rs`

- [ ] **Step 1: Write parser tests for GitLab token and app strategies**

Add these tests next to `parse_install_github_non_interactive_accepts_token_strategy()` in `lib/crates/fabro-cli/src/main.rs`:

```rust
#[test]
fn parse_install_gitlab_non_interactive_accepts_token_strategy() {
    let cli = Cli::try_parse_from([
        "fabro",
        "install",
        "gitlab",
        "--non-interactive",
        "--strategy",
        "token",
        "--base-url",
        "https://gitlab.ipt.example",
    ])
    .expect("should parse");
    match *cli.command.unwrap() {
        Commands::Install {
            args,
            command: Some(args::InstallCommand::Gitlab(gitlab_args)),
        } => {
            assert!(args.non_interactive);
            assert_eq!(gitlab_args.strategy, Some(args::InstallGitLabStrategyArg::Token));
            assert_eq!(gitlab_args.base_url.as_deref(), Some("https://gitlab.ipt.example"));
            assert!(gitlab_args.client_id.is_none());
        }
        _ => panic!("unexpected command variant"),
    }
}

#[test]
fn parse_install_gitlab_non_interactive_accepts_app_strategy() {
    let cli = Cli::try_parse_from([
        "fabro",
        "install",
        "gitlab",
        "--non-interactive",
        "--strategy",
        "app",
        "--base-url",
        "https://gitlab.ipt.example",
        "--client-id",
        "gitlab-client",
        "--allowed-username",
        "alice",
        "--allowed-group",
        "platform/fabro-admins",
    ])
    .expect("should parse");
    match *cli.command.unwrap() {
        Commands::Install {
            args,
            command: Some(args::InstallCommand::Gitlab(gitlab_args)),
        } => {
            assert!(args.non_interactive);
            assert_eq!(gitlab_args.strategy, Some(args::InstallGitLabStrategyArg::App));
            assert_eq!(gitlab_args.base_url.as_deref(), Some("https://gitlab.ipt.example"));
            assert_eq!(gitlab_args.client_id.as_deref(), Some("gitlab-client"));
            assert_eq!(gitlab_args.allowed_usernames, vec!["alice"]);
            assert_eq!(gitlab_args.allowed_groups, vec!["platform/fabro-admins"]);
        }
        _ => panic!("unexpected command variant"),
    }
}
```

- [ ] **Step 2: Run parser tests and verify failure**

Run:

```bash
cargo nextest run -p fabro-cli parse_install_gitlab_non_interactive_accepts
```

Expected: FAIL because `InstallCommand::Gitlab`, `InstallGitLabStrategyArg`, and `InstallGitlabArgs` do not exist yet.

- [ ] **Step 3: Add the argument structs**

In `lib/crates/fabro-cli/src/args.rs`, add this enum near `InstallGitHubStrategyArg`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum InstallGitLabStrategyArg {
    #[value(name = "token")]
    Token,
    App,
}
```

Then update `InstallCommand`:

```rust
#[derive(Subcommand, Debug, Clone)]
pub(crate) enum InstallCommand {
    /// Configure GitHub integration (token or GitHub App)
    Github(InstallGithubArgs),

    /// Configure GitLab integration (token or OAuth app)
    Gitlab(InstallGitlabArgs),
}
```

Add this struct below `InstallGithubArgs`:

```rust
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct InstallGitlabArgs {
    /// GitLab authentication strategy (requires --non-interactive)
    #[arg(long)]
    pub(crate) strategy: Option<InstallGitLabStrategyArg>,

    /// GitLab base URL, for example https://gitlab.com or https://gitlab.example.com
    #[arg(long)]
    pub(crate) base_url: Option<String>,

    /// GitLab OAuth application client ID (app only, requires --non-interactive)
    #[arg(long)]
    pub(crate) client_id: Option<String>,

    /// GitLab username allowed to log in (app only, repeatable)
    #[arg(long = "allowed-username")]
    pub(crate) allowed_usernames: Vec<String>,

    /// GitLab group full path allowed to log in (app only, repeatable)
    #[arg(long = "allowed-group")]
    pub(crate) allowed_groups: Vec<String>,

    /// Read the GitLab token from stdin
    #[arg(long, conflicts_with = "token_env")]
    pub(crate) token_stdin: bool,

    /// Read the GitLab token from this environment variable
    #[arg(long)]
    pub(crate) token_env: Option<String>,

    /// Read the GitLab OAuth client secret from stdin (app only)
    #[arg(long, conflicts_with = "client_secret_env")]
    pub(crate) client_secret_stdin: bool,

    /// Read the GitLab OAuth client secret from this environment variable (app only)
    #[arg(long)]
    pub(crate) client_secret_env: Option<String>,
}
```

- [ ] **Step 4: Update command-name mapping**

In `lib/crates/fabro-cli/src/args.rs`, update the install command name match so the GitLab subcommand reports as `install gitlab`:

```rust
Commands::Install { command, .. } => match command {
    Some(InstallCommand::Github(_)) => "install github",
    Some(InstallCommand::Gitlab(_)) => "install gitlab",
    None => "install",
},
```

- [ ] **Step 5: Run parser tests and verify pass**

Run:

```bash
cargo nextest run -p fabro-cli parse_install_gitlab_non_interactive_accepts
```

Expected: PASS.

- [ ] **Step 6: Update help snapshots**

In `lib/crates/fabro-cli/tests/it/cmd/install.rs`, update the `help()` snapshot to include:

```text
  gitlab  Configure GitLab integration (token or OAuth app)
```

Add a new test after `github_help()`:

```rust
#[test]
fn gitlab_help() {
    let context = test_context!();
    let mut cmd = context.install();
    cmd.args(["gitlab", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Configure GitLab integration (token or OAuth app)

    Usage: fabro install gitlab [OPTIONS]

    Options:
          --json                                      Output as JSON [env: FABRO_JSON=]
          --strategy <STRATEGY>                      GitLab authentication strategy (requires --non-interactive) [possible values: token, app]
          --debug                                     Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --base-url <BASE_URL>                      GitLab base URL, for example https://gitlab.com or https://gitlab.example.com
          --client-id <CLIENT_ID>                    GitLab OAuth application client ID (app only, requires --non-interactive)
          --allowed-username <ALLOWED_USERNAMES>     GitLab username allowed to log in (app only, repeatable)
          --allowed-group <ALLOWED_GROUPS>           GitLab group full path allowed to log in (app only, repeatable)
          --token-stdin                              Read the GitLab token from stdin
          --token-env <TOKEN_ENV>                    Read the GitLab token from this environment variable
          --client-secret-stdin                      Read the GitLab OAuth client secret from stdin (app only)
          --client-secret-env <CLIENT_SECRET_ENV>    Read the GitLab OAuth client secret from this environment variable (app only)
          --no-upgrade-check                         Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --non-interactive                          Run install without prompts; use hidden scripted flags for inputs
          --quiet                                    Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                                  Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                                      Print help
    ----- stderr -----
    ");
}
```

- [ ] **Step 7: Run help tests**

Run:

```bash
cargo nextest run -p fabro-cli 'cmd::install::help|cmd::install::gitlab_help'
```

Expected: PASS after adjusting spacing if clap renders a slightly different alignment. Review with `cargo insta pending-snapshots` before accepting any snapshot update.

- [ ] **Step 8: Commit**

```bash
git add lib/crates/fabro-cli/src/args.rs lib/crates/fabro-cli/src/main.rs lib/crates/fabro-cli/tests/it/cmd/install.rs
git commit -m "feat(cli): add gitlab install command surface"
```

## Task 2: Add GitLab Validation and Secret Input Helpers

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/install.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/install.rs`

- [ ] **Step 1: Write validation tests**

Add these tests near `github_scripted_flags_require_non_interactive()` in `lib/crates/fabro-cli/tests/it/cmd/install.rs`:

```rust
#[test]
fn gitlab_requires_prior_install() {
    let context = test_context!();
    std::fs::remove_file(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let output = context
        .command()
        .args(["install", "gitlab"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No settings.toml found. Run `fabro install` first."));
}

#[test]
fn gitlab_scripted_flags_require_non_interactive() {
    let context = test_context!();
    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let output = context
        .command()
        .args(["install", "gitlab", "--strategy", "token"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--strategy requires --non-interactive"));
}

#[test]
fn gitlab_non_interactive_requires_strategy() {
    let context = test_context!();
    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let output = context
        .command()
        .args(["install", "gitlab", "--non-interactive"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("install gitlab --non-interactive requires --strategy"));
}

#[test]
fn gitlab_token_requires_base_url_and_token_source() {
    let context = test_context!();
    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let output = context
        .command()
        .args([
            "install",
            "gitlab",
            "--non-interactive",
            "--strategy",
            "token",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("install gitlab token strategy requires --base-url"));
}
```

- [ ] **Step 2: Run validation tests and verify failure**

Run:

```bash
cargo nextest run -p fabro-cli 'cmd::install::gitlab_'
```

Expected: FAIL because the command is parsed but not handled.

- [ ] **Step 3: Import GitLab args and install helpers**

In `lib/crates/fabro-cli/src/commands/install.rs`, extend the imports:

```rust
use fabro_install::{
    GITHUB_APP_VAULT_KEYS, GITHUB_INSTALL_SECRET_KEYS, GITLAB_APP_VAULT_KEYS,
    GITLAB_INSTALL_SECRET_KEYS, GitlabAppInstallSettings, InstallListenConfig,
    InstallPersistencePlan, PendingSettingsWrite, VaultSecretWrite, merge_server_settings,
    prepare_dev_token_write_for_install, write_gitlab_app_settings, write_gitlab_token_settings,
    write_token_settings,
};
```

Update the args import:

```rust
use crate::args::{
    DoctorArgs, InstallArgs, InstallCommand, InstallGitHubStrategyArg, InstallGitLabStrategyArg,
    InstallGithubArgs, InstallGitlabArgs,
};
```

- [ ] **Step 4: Dispatch the subcommand**

Update `execute()`:

```rust
pub(crate) async fn execute(
    args: &InstallArgs,
    command: Option<InstallCommand>,
    ctx: &CommandContext,
) -> Result<()> {
    match command {
        None => run_install(args, ctx).await,
        Some(InstallCommand::Github(github_args)) => {
            run_install_github_command(args, &github_args, ctx).await
        }
        Some(InstallCommand::Gitlab(gitlab_args)) => {
            run_install_gitlab_command(args, &gitlab_args, ctx).await
        }
    }
}
```

- [ ] **Step 5: Add secret-source helper**

Add this enum near `ApiKeySource` or near the GitLab install helpers:

```rust
enum SecretInputSource {
    Stdin,
    EnvVar(String),
}

impl SecretInputSource {
    fn read(self, label: &str) -> Result<String> {
        match self {
            SecretInputSource::Stdin => {
                let mut value = String::new();
                std::io::stdin()
                    .read_line(&mut value)
                    .with_context(|| format!("failed to read {label} from stdin"))?;
                Ok(value.trim_end_matches(['\r', '\n']).to_string())
            }
            SecretInputSource::EnvVar(name) => std::env::var(&name)
                .with_context(|| format!("{label} environment variable {name} is not set")),
        }
    }
}
```

If `std::io::Read` or `std::io::BufRead` is not already imported, use fully-qualified calls or add `use std::io::Read as _;` and read all stdin with `read_to_string` instead:

```rust
let mut value = String::new();
std::io::stdin()
    .read_to_string(&mut value)
    .with_context(|| format!("failed to read {label} from stdin"))?;
Ok(value.trim_end_matches(['\r', '\n']).to_string())
```

- [ ] **Step 6: Add GitLab argument validation**

Add this function near `validate_install_github_non_interactive()`:

```rust
fn validate_install_gitlab_non_interactive(
    gitlab_args: &InstallGitlabArgs,
    non_interactive: bool,
) -> Result<()> {
    if !non_interactive {
        if gitlab_args.strategy.is_some() {
            bail!("--strategy requires --non-interactive");
        }
        if gitlab_args.base_url.is_some() {
            bail!("--base-url requires --non-interactive");
        }
        if gitlab_args.client_id.is_some() {
            bail!("--client-id requires --non-interactive");
        }
        if !gitlab_args.allowed_usernames.is_empty() {
            bail!("--allowed-username requires --non-interactive");
        }
        if !gitlab_args.allowed_groups.is_empty() {
            bail!("--allowed-group requires --non-interactive");
        }
        if gitlab_args.token_stdin || gitlab_args.token_env.is_some() {
            bail!("--token-stdin and --token-env require --non-interactive");
        }
        if gitlab_args.client_secret_stdin || gitlab_args.client_secret_env.is_some() {
            bail!("--client-secret-stdin and --client-secret-env require --non-interactive");
        }
        return Ok(());
    }

    match gitlab_args.strategy {
        Some(InstallGitLabStrategyArg::Token) => {
            anyhow::ensure!(
                gitlab_args.base_url.is_some(),
                "install gitlab token strategy requires --base-url"
            );
            anyhow::ensure!(
                gitlab_args.token_stdin ^ gitlab_args.token_env.is_some(),
                "install gitlab token strategy requires exactly one of --token-stdin or --token-env"
            );
            anyhow::ensure!(
                gitlab_args.client_id.is_none()
                    && gitlab_args.allowed_usernames.is_empty()
                    && gitlab_args.allowed_groups.is_empty()
                    && !gitlab_args.client_secret_stdin
                    && gitlab_args.client_secret_env.is_none(),
                "GitLab app options are only supported with --strategy app"
            );
        }
        Some(InstallGitLabStrategyArg::App) => {
            anyhow::ensure!(
                gitlab_args.base_url.is_some(),
                "install gitlab app strategy requires --base-url"
            );
            anyhow::ensure!(
                gitlab_args.client_id.is_some(),
                "install gitlab app strategy requires --client-id"
            );
            anyhow::ensure!(
                gitlab_args.token_stdin ^ gitlab_args.token_env.is_some(),
                "install gitlab app strategy requires exactly one of --token-stdin or --token-env"
            );
            anyhow::ensure!(
                gitlab_args.client_secret_stdin ^ gitlab_args.client_secret_env.is_some(),
                "install gitlab app strategy requires exactly one of --client-secret-stdin or --client-secret-env"
            );
            anyhow::ensure!(
                !gitlab_args.allowed_usernames.is_empty() || !gitlab_args.allowed_groups.is_empty(),
                "install gitlab app strategy requires at least one --allowed-username or --allowed-group"
            );
        }
        None => bail!("install gitlab --non-interactive requires --strategy"),
    }

    Ok(())
}
```

- [ ] **Step 7: Add GitLab base URL and token validation**

Add these helpers in `lib/crates/fabro-cli/src/commands/install.rs`, close to the GitLab command implementation:

```rust
fn validate_install_gitlab_base_url(raw: &str) -> Result<String> {
    let url = fabro_http::Url::parse(raw).context("GitLab base_url is invalid")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "GitLab base_url must use http or https"
    );
    if url.scheme() == "http" {
        let host = url.host_str().unwrap_or_default();
        let local_or_test = matches!(host, "localhost" | "127.0.0.1" | "::1")
            || host.ends_with(".localhost")
            || host.ends_with(".test");
        anyhow::ensure!(
            local_or_test,
            "GitLab base_url must use https unless it points to localhost or a test host"
        );
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

#[derive(serde::Deserialize)]
struct GitlabUserResponse {
    username: String,
}

async fn validate_gitlab_token(base_url: &str, token: &str) -> Result<String> {
    let base_url = validate_install_gitlab_base_url(base_url)?;
    let base = fabro_gitlab::repository::GitLabBaseUrl::parse(&base_url)
        .map_err(|err| anyhow::anyhow!("GitLab base_url is invalid: {err}"))?;
    let client = fabro_http::http_client()?;
    let response = client
        .get(fabro_gitlab::oauth::user_url(&base))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(anyhow::Error::new)?;
    if !response.status().is_success() {
        bail!("GitLab returned {}", response.status());
    }
    let body: GitlabUserResponse = response
        .json()
        .await
        .context("Failed to parse GitLab user response")?;
    Ok(body.username)
}
```

- [ ] **Step 8: Stub GitLab command enough for validation**

Add this function body first:

```rust
async fn run_install_gitlab_command(
    args: &InstallArgs,
    gitlab_args: &InstallGitlabArgs,
    ctx: &CommandContext,
) -> Result<()> {
    if ctx.explicit_json_requested() && !args.non_interactive {
        bail!("--json is only supported for install with --non-interactive");
    }
    validate_install_gitlab_non_interactive(gitlab_args, args.non_interactive)?;

    let fabro_dir = fabro_util::Home::from_env().root().to_path_buf();
    let config_path = fabro_dir.join(SETTINGS_CONFIG_FILENAME);
    if !config_path.exists() {
        bail!("No settings.toml found. Run `fabro install` first.");
    }

    bail!("GitLab install persistence is not implemented yet")
}
```

- [ ] **Step 9: Run validation tests**

Run:

```bash
cargo nextest run -p fabro-cli 'cmd::install::gitlab_requires_prior_install|cmd::install::gitlab_scripted_flags_require_non_interactive|cmd::install::gitlab_non_interactive_requires_strategy|cmd::install::gitlab_token_requires_base_url_and_token_source'
```

Expected: PASS for validation tests. The stub error is acceptable only for paths that pass validation and reach persistence.

- [ ] **Step 10: Commit**

```bash
git add lib/crates/fabro-cli/src/commands/install.rs lib/crates/fabro-cli/tests/it/cmd/install.rs
git commit -m "feat(cli): validate gitlab install inputs"
```

## Task 3: Implement Token Mode Persistence

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/install.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/install.rs`

- [ ] **Step 1: Write token persistence test**

Add this test near `github_non_interactive_token_reconfigures_existing_app_install()`:

```rust
#[test]
fn gitlab_non_interactive_token_reconfigures_existing_app_install() {
    let mut context = test_context!();
    let storage_dir = context.home_dir.join("gitlab-install-storage");
    context.manage_storage_dir(&storage_dir);
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token", "gitlab"]

[server.auth.gitlab]
allowed_usernames = ["alice"]
allowed_groups = ["platform/fabro-admins"]

[server.integrations.gitlab]
enabled = true
strategy = "app"
base_url = "https://gitlab.old.example"
client_id = "gitlab-client"

[project.metadata]
mode = "keep-me"
"#,
            storage_dir.display()
        ),
    );

    let server_env_path = Storage::new(&storage_dir).runtime_directory().env_path();
    envfile::write_env_file(
        &server_env_path,
        &std::collections::HashMap::from([
            ("GITLAB_APP_CLIENT_SECRET".to_string(), "client-secret".to_string()),
            ("KEEP_ME".to_string(), "1".to_string()),
        ]),
    )
    .unwrap();
    let mut stale_vault = Vault::load(Storage::new(&storage_dir).secrets_path()).unwrap();
    stale_vault
        .set("GITLAB_APP_CLIENT_SECRET", "client-secret", SecretType::Token, None)
        .unwrap();

    let gitlab = httpmock::MockServer::start();
    gitlab.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/api/v4/user")
            .header("authorization", "Bearer glpat-new-token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"username":"alice"}"#);
    });

    let output = context
        .command()
        .env("GITLAB_TOKEN_FOR_INSTALL", "glpat-new-token")
        .args([
            "install",
            "gitlab",
            "--non-interactive",
            "--strategy",
            "token",
            "--base-url",
            &gitlab.base_url(),
            "--token-env",
            "GITLAB_TOKEN_FOR_INSTALL",
        ])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "{output:?}");

    let settings = std::fs::read_to_string(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&settings).unwrap();
    let gitlab_settings = parsed
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("integrations"))
        .and_then(toml::Value::as_table)
        .and_then(|integrations| integrations.get("gitlab"))
        .and_then(toml::Value::as_table)
        .expect("server.integrations.gitlab should exist");
    assert_eq!(
        gitlab_settings.get("enabled").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        gitlab_settings.get("strategy").and_then(toml::Value::as_str),
        Some("token")
    );
    assert_eq!(
        gitlab_settings.get("base_url").and_then(toml::Value::as_str),
        Some(gitlab.base_url().as_str())
    );
    assert!(!gitlab_settings.contains_key("client_id"));

    let auth = parsed
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("auth"))
        .and_then(toml::Value::as_table)
        .expect("server.auth should exist");
    let methods = auth
        .get("methods")
        .and_then(toml::Value::as_array)
        .expect("server.auth.methods should exist");
    assert_eq!(
        methods
            .iter()
            .map(|value| value.as_str().expect("auth method should be a string"))
            .collect::<Vec<_>>(),
        vec!["dev-token"]
    );
    assert!(auth.get("gitlab").is_none());

    assert_eq!(
        parsed
            .get("project")
            .and_then(toml::Value::as_table)
            .and_then(|project| project.get("metadata"))
            .and_then(toml::Value::as_table)
            .and_then(|metadata| metadata.get("mode"))
            .and_then(toml::Value::as_str),
        Some("keep-me")
    );

    let server_env = envfile::read_env_file(&server_env_path).unwrap();
    assert!(!server_env.contains_key("GITLAB_APP_CLIENT_SECRET"));
    assert_eq!(server_env.get("KEEP_ME").map(String::as_str), Some("1"));

    let vault = Vault::load(Storage::new(&storage_dir).secrets_path()).unwrap();
    assert_eq!(vault.get("GITLAB_TOKEN"), Some("glpat-new-token"));
    assert_eq!(vault.get("GITLAB_APP_CLIENT_SECRET"), None);
    assert_eq!(
        vault
            .get_entry("GITLAB_TOKEN")
            .map(|entry| entry.secret_type),
        Some(SecretType::Token)
    );
}
```

- [ ] **Step 2: Run token persistence test and verify failure**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install::gitlab_non_interactive_token_reconfigures_existing_app_install
```

Expected: FAIL with the stub `"GitLab install persistence is not implemented yet"`.

- [ ] **Step 3: Implement GitLab selection enum**

Add near `GitHubInstallSelection`:

```rust
enum GitLabInstallSelection {
    Token {
        base_url: String,
        token:    String,
        username: String,
    },
    App {
        base_url:          String,
        client_id:         String,
        client_secret:     String,
        token:             String,
        username:          String,
        allowed_usernames: Vec<String>,
        allowed_groups:    Vec<String>,
    },
}
```

- [ ] **Step 4: Implement GitLab selection collection**

Add this function near `choose_install_github_selection()`:

```rust
async fn choose_install_gitlab_selection(
    install_args: &InstallArgs,
    gitlab_args: &InstallGitlabArgs,
) -> Result<GitLabInstallSelection> {
    validate_install_gitlab_non_interactive(gitlab_args, install_args.non_interactive)?;
    anyhow::ensure!(
        install_args.non_interactive,
        "interactive `fabro install gitlab` is not implemented yet; rerun with --non-interactive"
    );

    let base_url = validate_install_gitlab_base_url(
        gitlab_args
            .base_url
            .as_deref()
            .context("install gitlab requires --base-url")?,
    )?;
    let token_source = if gitlab_args.token_stdin {
        SecretInputSource::Stdin
    } else if let Some(name) = &gitlab_args.token_env {
        SecretInputSource::EnvVar(name.clone())
    } else {
        bail!("install gitlab requires exactly one of --token-stdin or --token-env");
    };
    let token = token_source.read("GitLab token")?;
    let username = validate_gitlab_token(&base_url, &token).await?;

    match gitlab_args.strategy {
        Some(InstallGitLabStrategyArg::Token) => Ok(GitLabInstallSelection::Token {
            base_url,
            token,
            username,
        }),
        Some(InstallGitLabStrategyArg::App) => {
            let client_secret_source = if gitlab_args.client_secret_stdin {
                SecretInputSource::Stdin
            } else if let Some(name) = &gitlab_args.client_secret_env {
                SecretInputSource::EnvVar(name.clone())
            } else {
                bail!(
                    "install gitlab app strategy requires exactly one of --client-secret-stdin or --client-secret-env"
                );
            };
            Ok(GitLabInstallSelection::App {
                base_url,
                client_id: gitlab_args
                    .client_id
                    .clone()
                    .context("install gitlab app strategy requires --client-id")?,
                client_secret: client_secret_source.read("GitLab OAuth client secret")?,
                token,
                username,
                allowed_usernames: gitlab_args.allowed_usernames.clone(),
                allowed_groups: gitlab_args.allowed_groups.clone(),
            })
        }
        None => bail!("install gitlab --non-interactive requires --strategy"),
    }
}
```

- [ ] **Step 5: Add reusable persistence struct**

Either reuse `PendingGitHubInstallWrite` by renaming it to `PendingSourceControlInstallWrite`, or add a sibling struct. Prefer renaming because the fields are provider-neutral:

```rust
struct PendingSourceControlInstallWrite<'a> {
    settings_write:    PendingSettingsWrite<'a>,
    server_env_set:    Vec<(String, String)>,
    server_env_remove: Vec<&'static str>,
    vault_set:         Vec<VaultSecretWrite>,
    vault_remove:      Vec<&'static str>,
}
```

Rename `persist_github_install_changes` to:

```rust
fn persist_source_control_install_changes(
    storage_dir: &Path,
    writes: &PendingSourceControlInstallWrite<'_>,
) -> Result<()> {
    let server_env_path = Storage::new(storage_dir).runtime_directory().env_path();
    let previous_server_env = std::fs::read_to_string(&server_env_path).ok();

    match (InstallPersistencePlan {
        storage_dir,
        settings_write: Some(writes.settings_write),
        server_env_writes: server_env_updates(&writes.server_env_set),
        server_env_removals: server_env_removals(&writes.server_env_remove),
        dev_token_write: None,
        vault_writes: writes.vault_set.clone(),
        vault_removals: writes
            .vault_remove
            .iter()
            .map(|key| (*key).to_string())
            .collect(),
    }
    .persist_direct())
    {
        Ok(()) => Ok(()),
        Err(err) => {
            let err = anyhow::Error::from(err);
            match restore_optional_file(&server_env_path, previous_server_env.as_deref()) {
                Ok(()) => Err(err),
                Err(restore_err) => {
                    Err(err.context(format!("server env rollback failure: {restore_err}")))
                }
            }
        }
    }
}
```

Update the GitHub call site to use the renamed function and struct.

- [ ] **Step 6: Implement token mode in `run_install_gitlab_command`**

Replace the stub body after reading settings with this token-capable implementation:

```rust
let json = ctx.json_output();
let s = Styles::detect_stderr();
let fabro_dir = fabro_util::Home::from_env().root().to_path_buf();
let config_path = fabro_dir.join(SETTINGS_CONFIG_FILENAME);
if !config_path.exists() {
    bail!("No settings.toml found. Run `fabro install` first.");
}

let existing_config_contents =
    std::fs::read_to_string(&config_path).context("failed to read existing settings.toml")?;
let storage_dir = args
    .storage_dir
    .clone_path()
    .or_else(|| local_server::storage_dir_from_toml(&existing_config_contents).ok())
    .unwrap_or_else(default_storage_dir);
let server_was_running =
    ServerDaemon::load_running(&Storage::new(&storage_dir).runtime_directory())?.is_some();
let mut doc: toml::Value = toml::from_str(&existing_config_contents)
    .context("failed to parse existing settings.toml")?;

let selection = choose_install_gitlab_selection(args, gitlab_args).await?;
let server_env_set = Vec::new();
let mut server_env_remove = Vec::new();
let mut vault_set = Vec::new();
let mut vault_remove = Vec::new();

match selection {
    GitLabInstallSelection::Token {
        base_url,
        token,
        username,
    } => {
        write_gitlab_token_settings(&mut doc, &base_url)?;
        vault_set.push(VaultSecretWrite {
            name:        fabro_static::EnvVars::GITLAB_TOKEN.to_string(),
            value:       token,
            secret_type: VaultSecretType::Token,
            description: None,
        });
        server_env_remove.extend(GITLAB_INSTALL_SECRET_KEYS.iter().copied());
        vault_remove.extend(GITLAB_APP_VAULT_KEYS.iter().copied());
        if !json {
            fabro_util::printerr!(
                ctx.printer(),
                "  {} GitLab token configured for {username}",
                s.green.apply_to("✔")
            );
        }
    }
    GitLabInstallSelection::App { .. } => {
        bail!("GitLab app persistence is not implemented yet");
    }
}

let settings_toml = toml::to_string_pretty(&doc)?;
persist_source_control_install_changes(&storage_dir, &PendingSourceControlInstallWrite {
    settings_write: PendingSettingsWrite {
        path:              &config_path,
        contents:          settings_toml.as_str(),
        previous_contents: Some(existing_config_contents.as_str()),
    },
    server_env_set,
    server_env_remove,
    vault_set,
    vault_remove,
})?;

if let Some(restart_outcome) =
    maybe_restart_server_after_github_install(&storage_dir, &config_path, server_was_running).await
{
    match restart_outcome {
        InstallServerRestartOutcome::Started(bind) => {
            fabro_util::printerr!(
                ctx.printer(),
                "  {} Server running at http://{}",
                s.green.apply_to("✔"),
                bind
            );
            let methods = fabro_config::ServerSettingsBuilder::from_toml(&settings_toml)
                .ok()
                .map(|settings| settings.server.auth.methods)
                .unwrap_or_default();
            let token = methods
                .contains(&ServerAuthMethod::DevToken)
                .then(|| {
                    dev_token::read_dev_token_file(
                        &Storage::new(&storage_dir)
                            .runtime_directory()
                            .dev_token_path(),
                    )
                })
                .flatten();
            print_auth_status(&methods, token.as_deref(), &s, ctx.printer());
            fabro_util::printerr!(ctx.printer(), "");
        }
        InstallServerRestartOutcome::Failed(err) => {
            fabro_util::printerr!(
                ctx.printer(),
                "  {} Failed to restart server: {err}",
                s.yellow.apply_to("Warning:")
            );
        }
    }
}

if json {
    emit_install_json_event(&install_complete_event())?;
}
Ok(())
```

If the helper name `maybe_restart_server_after_github_install` feels too provider-specific, rename it to `maybe_restart_server_after_source_control_install` and update the GitHub call site in the same step.

- [ ] **Step 7: Run token persistence test**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install::gitlab_non_interactive_token_reconfigures_existing_app_install
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add lib/crates/fabro-cli/src/commands/install.rs lib/crates/fabro-cli/tests/it/cmd/install.rs
git commit -m "feat(cli): persist gitlab token install"
```

## Task 4: Implement GitLab App Mode Persistence

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/install.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/install.rs`

- [ ] **Step 1: Write app persistence test**

Add this test after the token persistence test:

```rust
#[test]
fn gitlab_non_interactive_app_reconfigures_existing_token_install() {
    let mut context = test_context!();
    let storage_dir = context.home_dir.join("gitlab-app-install-storage");
    context.manage_storage_dir(&storage_dir);
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token"]

[server.integrations.gitlab]
enabled = true
strategy = "token"
base_url = "https://gitlab.old.example"

[project.metadata]
mode = "keep-me"
"#,
            storage_dir.display()
        ),
    );

    let server_env_path = Storage::new(&storage_dir).runtime_directory().env_path();
    envfile::write_env_file(
        &server_env_path,
        &std::collections::HashMap::from([
            ("GITLAB_TOKEN".to_string(), "stale-env-token".to_string()),
            ("KEEP_ME".to_string(), "1".to_string()),
        ]),
    )
    .unwrap();

    let gitlab = httpmock::MockServer::start();
    gitlab.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/api/v4/user")
            .header("authorization", "Bearer glpat-app-token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"username":"alice"}"#);
    });

    let output = context
        .command()
        .env("GITLAB_TOKEN_FOR_INSTALL", "glpat-app-token")
        .env("GITLAB_CLIENT_SECRET_FOR_INSTALL", "oauth-secret")
        .args([
            "install",
            "gitlab",
            "--non-interactive",
            "--strategy",
            "app",
            "--base-url",
            &gitlab.base_url(),
            "--client-id",
            "gitlab-client",
            "--client-secret-env",
            "GITLAB_CLIENT_SECRET_FOR_INSTALL",
            "--token-env",
            "GITLAB_TOKEN_FOR_INSTALL",
            "--allowed-username",
            "alice",
            "--allowed-group",
            "platform/fabro-admins",
        ])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "{output:?}");

    let settings = std::fs::read_to_string(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&settings).unwrap();
    let gitlab_settings = parsed
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("integrations"))
        .and_then(toml::Value::as_table)
        .and_then(|integrations| integrations.get("gitlab"))
        .and_then(toml::Value::as_table)
        .expect("server.integrations.gitlab should exist");
    assert_eq!(
        gitlab_settings.get("enabled").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        gitlab_settings.get("strategy").and_then(toml::Value::as_str),
        Some("app")
    );
    assert_eq!(
        gitlab_settings.get("base_url").and_then(toml::Value::as_str),
        Some(gitlab.base_url().as_str())
    );
    assert_eq!(
        gitlab_settings.get("client_id").and_then(toml::Value::as_str),
        Some("gitlab-client")
    );

    let auth = parsed
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("auth"))
        .and_then(toml::Value::as_table)
        .expect("server.auth should exist");
    let methods = auth
        .get("methods")
        .and_then(toml::Value::as_array)
        .expect("server.auth.methods should exist");
    assert_eq!(
        methods
            .iter()
            .map(|value| value.as_str().expect("auth method should be a string"))
            .collect::<Vec<_>>(),
        vec!["dev-token", "gitlab"]
    );
    let gitlab_auth = auth
        .get("gitlab")
        .and_then(toml::Value::as_table)
        .expect("server.auth.gitlab should exist");
    assert_eq!(
        gitlab_auth
            .get("allowed_usernames")
            .and_then(toml::Value::as_array)
            .expect("allowed usernames should exist")
            .iter()
            .map(|value| value.as_str().expect("username should be a string"))
            .collect::<Vec<_>>(),
        vec!["alice"]
    );
    assert_eq!(
        gitlab_auth
            .get("allowed_groups")
            .and_then(toml::Value::as_array)
            .expect("allowed groups should exist")
            .iter()
            .map(|value| value.as_str().expect("group should be a string"))
            .collect::<Vec<_>>(),
        vec!["platform/fabro-admins"]
    );

    let server_env = envfile::read_env_file(&server_env_path).unwrap();
    assert!(!server_env.contains_key("GITLAB_TOKEN"));
    assert_eq!(server_env.get("KEEP_ME").map(String::as_str), Some("1"));

    let vault = Vault::load(Storage::new(&storage_dir).secrets_path()).unwrap();
    assert_eq!(vault.get("GITLAB_TOKEN"), Some("glpat-app-token"));
    assert_eq!(vault.get("GITLAB_APP_CLIENT_SECRET"), Some("oauth-secret"));
    assert_eq!(
        vault
            .get_entry("GITLAB_APP_CLIENT_SECRET")
            .map(|entry| entry.secret_type),
        Some(SecretType::Token)
    );
}
```

- [ ] **Step 2: Run app persistence test and verify failure**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install::gitlab_non_interactive_app_reconfigures_existing_token_install
```

Expected: FAIL with `"GitLab app persistence is not implemented yet"`.

- [ ] **Step 3: Implement app mode match arm**

Replace the `GitLabInstallSelection::App { .. }` arm in `run_install_gitlab_command` with:

```rust
GitLabInstallSelection::App {
    base_url,
    client_id,
    client_secret,
    token,
    username,
    allowed_usernames,
    allowed_groups,
} => {
    write_gitlab_app_settings(&mut doc, GitlabAppInstallSettings {
        base_url,
        client_id,
        allowed_usernames,
        allowed_groups,
    })?;
    vault_set.push(VaultSecretWrite {
        name:        fabro_static::EnvVars::GITLAB_TOKEN.to_string(),
        value:       token,
        secret_type: VaultSecretType::Token,
        description: None,
    });
    vault_set.push(VaultSecretWrite {
        name:        fabro_static::EnvVars::GITLAB_APP_CLIENT_SECRET.to_string(),
        value:       client_secret,
        secret_type: VaultSecretType::Token,
        description: None,
    });
    server_env_remove.extend(GITLAB_INSTALL_SECRET_KEYS.iter().copied());
    if !json {
        fabro_util::printerr!(
            ctx.printer(),
            "  {} GitLab app configured for {username}",
            s.green.apply_to("✔")
        );
    }
}
```

- [ ] **Step 4: Run app persistence test**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install::gitlab_non_interactive_app_reconfigures_existing_token_install
```

Expected: PASS.

- [ ] **Step 5: Run all install command tests**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-cli/src/commands/install.rs lib/crates/fabro-cli/tests/it/cmd/install.rs
git commit -m "feat(cli): persist gitlab app install"
```

## Task 5: JSON Output and Documentation

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/install.rs`
- Modify: `lib/crates/fabro-cli/tests/it/cmd/install.rs`
- Modify: `docs/public/administration/server-configuration.mdx`

- [ ] **Step 1: Write JSON output test**

Add this test near existing JSON install tests:

```rust
#[test]
fn install_gitlab_json_requires_non_interactive() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "install", "gitlab"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is only supported for install with --non-interactive"));
}
```

- [ ] **Step 2: Run JSON test**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install::install_gitlab_json_requires_non_interactive
```

Expected: PASS.

- [ ] **Step 3: Ensure errors emit JSON in non-interactive JSON mode**

Update `run_install_gitlab_command` to mirror `run_install_github_command`:

```rust
async fn run_install_gitlab_command(
    args: &InstallArgs,
    gitlab_args: &InstallGitlabArgs,
    ctx: &CommandContext,
) -> Result<()> {
    let json = ctx.json_output();
    if ctx.explicit_json_requested() && !args.non_interactive {
        bail!("--json is only supported for install with --non-interactive");
    }

    let result = Box::pin(run_install_gitlab_inner(
        args,
        gitlab_args,
        json,
        ctx.printer(),
    ))
    .await;
    if json {
        let emit_result = match &result {
            Ok(()) => emit_install_json_event(&install_complete_event()),
            Err(err) => emit_install_json_event(&install_error_event(&err.to_string())),
        };
        if result.is_ok() {
            emit_result?;
        }
    }

    result
}
```

Move the current implementation body into:

```rust
async fn run_install_gitlab_inner(
    args: &InstallArgs,
    gitlab_args: &InstallGitlabArgs,
    json_output: bool,
    printer: Printer,
) -> Result<()> {
    /* existing implementation */
}
```

Inside the inner implementation, use `json_output` and `printer` instead of `ctx.json_output()` and `ctx.printer()`.

- [ ] **Step 4: Add docs**

In `docs/public/administration/server-configuration.mdx`, after the GitLab app-mode paragraph, add:

```mdx
You can reconfigure GitLab from an existing local install with the CLI:

```bash
fabro install gitlab --non-interactive \
  --strategy token \
  --base-url https://gitlab.example.com \
  --token-env GITLAB_TOKEN
```

For GitLab OAuth login, provide the OAuth application values and at least one allowed user or group:

```bash
fabro install gitlab --non-interactive \
  --strategy app \
  --base-url https://gitlab.example.com \
  --client-id "$GITLAB_CLIENT_ID" \
  --client-secret-env GITLAB_APP_CLIENT_SECRET \
  --token-env GITLAB_TOKEN \
  --allowed-group platform/fabro-admins
```

The `base_url` may point to a self-hosted GitLab instance. `http://` is accepted only for localhost and test hosts; production self-hosted instances should use `https://`.
```

- [ ] **Step 5: Run focused verification**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install
cargo nextest run -p fabro-cli parse_install_gitlab_non_interactive_accepts
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-cli/src/commands/install.rs lib/crates/fabro-cli/tests/it/cmd/install.rs docs/public/administration/server-configuration.mdx
git commit -m "docs: document gitlab install CLI"
```

## Task 6: Final Formatting and Verification

**Files:**
- Verify: entire workspace subset touched by this plan

- [ ] **Step 1: Format Rust**

Run:

```bash
cargo +nightly-2026-04-14 fmt --all
```

Expected: command exits 0.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo nextest run -p fabro-cli cmd::install
cargo nextest run -p fabro-cli parse_install_gitlab_non_interactive_accepts
cargo nextest run -p fabro-install write_gitlab
```

Expected: all tests PASS.

- [ ] **Step 3: Run broader CLI checks**

Run:

```bash
cargo nextest run -p fabro-cli
```

Expected: all tests PASS.

- [ ] **Step 4: Run clippy if time allows**

Run:

```bash
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Expected: PASS. If this is too slow for the session, record that it was not run and why.

- [ ] **Step 5: Review snapshots before accepting**

Run:

```bash
cargo insta pending-snapshots
```

Expected: either no pending snapshots or only the intended `install --help` snapshot changes. If pending snapshots exist, inspect them first, then accept only intended changes.

- [ ] **Step 6: Commit verification cleanup**

If formatting or snapshot acceptance changed files:

```bash
git add lib/crates/fabro-cli/src/args.rs lib/crates/fabro-cli/src/main.rs lib/crates/fabro-cli/src/commands/install.rs lib/crates/fabro-cli/tests/it/cmd/install.rs docs/public/administration/server-configuration.mdx
git commit -m "test(cli): verify gitlab install parity"
```

## Self-Review

- Spec coverage: The plan covers what `fabro install github` does by adding a parallel GitLab subcommand, covers self-hosted GitLab through `--base-url`, covers token mode, covers app/OAuth mode, and documents the GitLab/GitHub semantic difference.
- Placeholder scan: No `TBD`, `TODO`, or unspecified test tasks remain. The implementation snippets name concrete functions, files, commands, and expected outcomes.
- Type consistency: `InstallGitLabStrategyArg`, `InstallGitlabArgs`, `GitLabInstallSelection`, and the helper function names are consistent across tasks. Existing `fabro_install` functions `write_gitlab_token_settings` and `write_gitlab_app_settings` are reused exactly.
