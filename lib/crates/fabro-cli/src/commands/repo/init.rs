use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::task::spawn_blocking;

use crate::cli_config::load_cli_settings;
use crate::shared::github::build_github_app_credentials;

pub(super) fn git_repo_root() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!("not a git repository — run `git init` first");
    }
    Ok(PathBuf::from(
        String::from_utf8(output.stdout)
            .context("git output was not valid UTF-8")?
            .trim(),
    ))
}

pub(crate) async fn run_init() -> Result<()> {
    let repo_root = git_repo_root()?;

    let fabro_toml = repo_root.join("fabro.toml");
    if fabro_toml.exists() {
        bail!(
            "already initialized — fabro.toml exists at {}",
            fabro_toml.display()
        );
    }

    // Create fabro.toml
    std::fs::write(
        &fabro_toml,
        "\
# Fabro project configuration
# https://docs.fabro.computer/getting-started/quick-start

version = 1

[fabro]
root = \"fabro/\"

# Disable retrospective analysis after workflow runs:
# retro = false

# Auto-create pull requests on successful workflow runs.
[pull_request]
enabled = true
draft = true
# auto_merge = true
",
    )
    .with_context(|| format!("failed to write {}", fabro_toml.display()))?;

    let green = console::Style::new().green();
    let bold = console::Style::new().bold();
    let dim = console::Style::new().dim();
    eprintln!("  {} {}", green.apply_to("✔"), dim.apply_to("fabro.toml"));

    // Create hello workflow directory
    let workflow_dir = repo_root.join("fabro/workflows/hello");
    std::fs::create_dir_all(&workflow_dir)
        .with_context(|| format!("failed to create {}", workflow_dir.display()))?;

    // Create workflow.fabro
    let dot_path = workflow_dir.join("workflow.fabro");
    std::fs::write(
        &dot_path,
        r#"digraph Hello {
    graph [goal="Say hello and demonstrate a basic Fabro workflow"]
    rankdir=LR

    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    greet [label="Greet", prompt="Say hello! Introduce yourself and explain that this is a test of the Fabro workflow engine."]

    start -> greet -> exit
}
"#,
    )
    .with_context(|| format!("failed to write {}", dot_path.display()))?;
    eprintln!(
        "  {} {}",
        green.apply_to("✔"),
        dim.apply_to("fabro/workflows/hello/workflow.fabro")
    );

    // Create workflow.toml
    let toml_path = workflow_dir.join("workflow.toml");
    std::fs::write(
        &toml_path,
        "version = 1\ngraph = \"workflow.fabro\"\n\n[sandbox]\nprovider = \"local\"\n",
    )
    .with_context(|| format!("failed to write {}", toml_path.display()))?;
    eprintln!(
        "  {} {}",
        green.apply_to("✔"),
        dim.apply_to("fabro/workflows/hello/workflow.toml")
    );

    eprintln!(
        "\n{} Run a workflow with:\n\n  {}",
        bold.apply_to("Project initialized!"),
        console::Style::new()
            .cyan()
            .bold()
            .apply_to("fabro run hello")
    );

    check_github_app_installation().await;

    Ok(())
}

async fn check_github_app_installation() {
    // Get the git remote origin URL
    let output = match std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => {
            let yellow = console::Style::new().yellow();
            let dim = console::Style::new().dim();
            eprintln!(
                "\n  {} No git remote found — skipping GitHub App check",
                yellow.apply_to("!")
            );
            eprintln!(
                "  {}",
                dim.apply_to(
                    "Run `git remote add origin <url>` then `fabro install` to set up the GitHub App"
                )
            );
            return;
        }
    };

    let remote_url = match String::from_utf8(output.stdout) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };

    // Convert SSH URL to HTTPS and parse owner/repo
    let https_url = fabro_github::ssh_url_to_https(&remote_url);
    let Ok((owner, repo)) = fabro_github::parse_github_owner_repo(&https_url) else {
        return; // Not a GitHub repo — skip silently
    };

    // Load CLI config to get app_id and slug
    let Ok(cli_settings) = load_cli_settings() else {
        return;
    };

    let app_id = if let Some(id) = cli_settings.app_id() {
        id.to_string()
    } else {
        eprintln!(
            "\n  Run {} to set up the GitHub App",
            console::Style::new()
                .cyan()
                .bold()
                .apply_to("fabro install")
        );
        return;
    };

    let slug = cli_settings.slug().map(String::from);

    // Build GitHub App credentials
    let Some(creds) = build_github_app_credentials(Some(&app_id)) else {
        eprintln!(
            "\n  Set {} to enable GitHub App integration",
            console::Style::new()
                .cyan()
                .bold()
                .apply_to("GITHUB_APP_PRIVATE_KEY")
        );
        return;
    };

    let jwt = match fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("\n  Warning: failed to sign GitHub App JWT: {e}");
            return;
        }
    };

    let client = reqwest::Client::new();

    match fabro_github::check_app_installed(
        &client,
        &jwt,
        &owner,
        &repo,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    {
        Ok(true) => {
            let green = console::Style::new().green();
            eprintln!(
                "\n  {} GitHub App is installed for {owner}/{repo}",
                green.apply_to("✔")
            );
        }
        Ok(false) => {
            let install_url = match &slug {
                Some(s) => format!("https://github.com/apps/{s}/installations/new"),
                None => format!("https://github.com/organizations/{owner}/settings/installations"),
            };

            let yellow = console::Style::new().yellow();

            // Best-effort: warn if the app is private and the repo belongs to a different owner.
            if let Ok(app_info) = fabro_github::get_authenticated_app(
                &client,
                &jwt,
                fabro_github::GITHUB_API_BASE_URL,
            )
            .await
            {
                let cross_owner = !app_info.owner.login.eq_ignore_ascii_case(&owner);
                let is_private = cross_owner
                    && fabro_github::is_app_public(
                        &client,
                        &app_info.slug,
                        fabro_github::GITHUB_API_BASE_URL,
                    )
                    .await
                        == Ok(false);

                if is_private {
                    eprintln!(
                        "\n  {} GitHub App \"{}\" is private but this repo belongs to a different owner ({}).",
                        yellow.apply_to("!"),
                        app_info.slug,
                        owner
                    );
                    eprintln!(
                        "    The app must be made public before it can be installed outside {}.",
                        app_info.owner.login
                    );
                    eprintln!(
                        "    Update visibility at: https://github.com/settings/apps/{}",
                        app_info.slug
                    );
                }
            }
            eprintln!(
                "\n  {} GitHub App is not installed for {owner}/{repo}",
                yellow.apply_to("!")
            );
            eprintln!("  Install at: {install_url}");

            // Only prompt if stdin is a terminal
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                eprintln!("  Press Enter to continue after installing...");
                let _ = spawn_blocking(|| {
                    let mut buf = String::new();
                    let _ = std::io::stdin().read_line(&mut buf);
                })
                .await;

                // Re-check after user presses Enter
                match fabro_github::check_app_installed(
                    &client,
                    &jwt,
                    &owner,
                    &repo,
                    fabro_github::GITHUB_API_BASE_URL,
                )
                .await
                {
                    Ok(true) => {
                        let green = console::Style::new().green();
                        eprintln!(
                            "  {} GitHub App is installed for {owner}/{repo}",
                            green.apply_to("✔")
                        );
                    }
                    Ok(false) => {
                        eprintln!("  GitHub App is still not installed.");
                        eprintln!("  Install at: {install_url}");
                    }
                    Err(e) => {
                        eprintln!("  Warning: could not re-check GitHub App installation: {e}");
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("\n  Warning: could not check GitHub App installation: {e}");
        }
    }
}
