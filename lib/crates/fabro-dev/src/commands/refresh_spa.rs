use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;
use walkdir::WalkDir;

#[derive(Debug, Args)]
pub(crate) struct RefreshSpaArgs {
    /// Repository root containing apps/fabro-web and lib/crates/fabro-spa.
    #[arg(long, hide = true)]
    root:       Option<PathBuf>,
    /// Skip bun run build and only mirror an existing dist directory.
    #[arg(long, hide = true)]
    skip_build: bool,
}

#[expect(
    clippy::print_stdout,
    reason = "dev refresh-spa command reports progress directly"
)]
pub(crate) fn refresh_spa(args: RefreshSpaArgs) -> Result<()> {
    let root = args.root.unwrap_or_else(workspace_root);
    let web_dir = root.join("apps/fabro-web");
    let dist_dir = web_dir.join("dist");
    let asset_dir = root.join("lib/crates/fabro-spa/assets");

    if !args.skip_build {
        println!("Running bun run build in apps/fabro-web...");
        run_bun_build(&web_dir)?;
    }

    mirror_dist(&dist_dir, &asset_dir)?;
    println!("Refreshed lib/crates/fabro-spa/assets");

    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev refresh-spa intentionally runs a synchronous Bun subprocess"
)]
fn run_bun_build(web_dir: &Path) -> Result<()> {
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(web_dir)
        .status()
        .with_context(|| format!("running bun run build in {}", web_dir.display()))?;
    if !status.success() {
        bail!("bun run build failed with {status}");
    }

    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev refresh-spa mirrors build output with synchronous filesystem operations"
)]
fn mirror_dist(dist_dir: &Path, asset_dir: &Path) -> Result<()> {
    if !dist_dir.is_dir() {
        bail!("apps/fabro-web/dist is missing; run `bun run build` before mirroring SPA assets");
    }

    if asset_dir.exists() {
        std::fs::remove_dir_all(asset_dir)
            .with_context(|| format!("removing {}", asset_dir.display()))?;
    }
    std::fs::create_dir_all(asset_dir)
        .with_context(|| format!("creating {}", asset_dir.display()))?;

    for entry in WalkDir::new(dist_dir) {
        let entry = entry.context("walking apps/fabro-web/dist")?;
        let source = entry.path();
        let relative = source
            .strip_prefix(dist_dir)
            .with_context(|| format!("{} is not under {}", source.display(), dist_dir.display()))?;
        if relative.as_os_str().is_empty() {
            continue;
        }

        let destination = asset_dir.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&destination)
                .with_context(|| format!("creating {}", destination.display()))?;
            continue;
        }

        if source.extension().and_then(|ext| ext.to_str()) == Some("map") {
            continue;
        }

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(source, &destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
    }

    Ok(())
}

fn workspace_root() -> PathBuf {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    root.pop();
    root.pop();
    root.pop();
    root
}
