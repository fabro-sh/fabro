use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::task::spawn_blocking;

use crate::args::{GlobalArgs, RepoInitArgs, ServerTargetArgs};
use crate::command_context::CommandContext;

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

pub(crate) async fn run_init(args: &RepoInitArgs, globals: &GlobalArgs) -> Result<Vec<String>> {
    let repo_root = git_repo_root()?;
    let mut created = Vec::new();

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
    created.push("fabro.toml".to_string());

    let green = console::Style::new().green();
    let bold = console::Style::new().bold();
    let dim = console::Style::new().dim();
    if !globals.json {
        eprintln!("  {} {}", green.apply_to("✔"), dim.apply_to("fabro.toml"));
    }

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
    created.push("fabro/workflows/hello/workflow.fabro".to_string());
    if !globals.json {
        eprintln!(
            "  {} {}",
            green.apply_to("✔"),
            dim.apply_to("fabro/workflows/hello/workflow.fabro")
        );
    }

    // Create workflow.toml
    let toml_path = workflow_dir.join("workflow.toml");
    std::fs::write(
        &toml_path,
        "version = 1\ngraph = \"workflow.fabro\"\n\n[sandbox]\nprovider = \"local\"\n",
    )
    .with_context(|| format!("failed to write {}", toml_path.display()))?;
    created.push("fabro/workflows/hello/workflow.toml".to_string());
    if !globals.json {
        eprintln!(
            "  {} {}",
            green.apply_to("✔"),
            dim.apply_to("fabro/workflows/hello/workflow.toml")
        );
    }

    if !globals.json {
        eprintln!(
            "\n{} Run a workflow with:\n\n  {}",
            bold.apply_to("Project initialized!"),
            console::Style::new()
                .cyan()
                .bold()
                .apply_to("fabro run hello")
        );
    }

    if !globals.json {
        check_github_app_installation(&args.target).await;
    }

    Ok(created)
}

async fn check_github_app_installation(target: &ServerTargetArgs) {
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

    let ctx = match CommandContext::for_target(target) {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("\n  Warning: could not resolve fabro server settings: {err}");
            return;
        }
    };

    let server = match ctx.server().await {
        Ok(server) => server,
        Err(err) => {
            eprintln!("\n  Warning: could not connect to fabro server: {err}");
            return;
        }
    };

    let check = match server
        .api()
        .get_github_repo()
        .owner(owner.clone())
        .name(repo.clone())
        .send()
        .await
    {
        Ok(response) => response.into_inner(),
        Err(err) => {
            eprintln!("\n  Warning: could not check GitHub App installation: {err}");
            return;
        }
    };

    if check.accessible {
        let green = console::Style::new().green();
        eprintln!(
            "\n  {} GitHub App is installed for {owner}/{repo}",
            green.apply_to("✔")
        );
        return;
    }

    let yellow = console::Style::new().yellow();
    eprintln!(
        "\n  {} GitHub App is not installed for {owner}/{repo}",
        yellow.apply_to("!")
    );
    if let Some(url) = &check.install_url {
        eprintln!("  Install at: {url}");
    }

    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("  Press Enter to continue after installing...");
        let _ = spawn_blocking(|| {
            let mut buf = String::new();
            let _ = std::io::stdin().read_line(&mut buf);
        })
        .await;

        match server
            .api()
            .get_github_repo()
            .owner(owner.clone())
            .name(repo.clone())
            .send()
            .await
        {
            Ok(response) => {
                let response = response.into_inner();
                if response.accessible {
                    let green = console::Style::new().green();
                    eprintln!(
                        "  {} GitHub App is installed for {owner}/{repo}",
                        green.apply_to("✔")
                    );
                } else {
                    eprintln!("  GitHub App is still not installed.");
                    if let Some(url) = &check.install_url {
                        eprintln!("  Install at: {url}");
                    }
                }
            }
            Err(err) => {
                eprintln!("  Warning: could not re-check GitHub App installation: {err}");
            }
        }
    }
}
