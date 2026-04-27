use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use clap::Args;
use walkdir::WalkDir;

use super::spa_refresh::{TempDir, mirror_dist, run_bun_build};
use super::workspace_root;

const DEFAULT_ASSET_BUDGET_BYTES: u64 = 15 * 1024 * 1024;
const DEFAULT_PAYLOAD_BUDGET_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct SpaCheckArgs {
    /// Repository root containing lib/crates/fabro-spa/assets.
    #[arg(long, hide = true)]
    root:                 Option<PathBuf>,
    /// Override the raw asset budget.
    #[arg(long, hide = true, default_value_t = DEFAULT_ASSET_BUDGET_BYTES)]
    asset_budget_bytes:   u64,
    /// Override the estimated gzip payload budget.
    #[arg(long, hide = true, default_value_t = DEFAULT_PAYLOAD_BUDGET_BYTES)]
    payload_budget_bytes: u64,
    /// Skip bun run build and compare an existing dist directory.
    #[arg(long, hide = true)]
    skip_build:           bool,
}

#[expect(
    clippy::print_stdout,
    reason = "dev spa check command reports measured budgets directly"
)]
pub(crate) fn spa_check(args: SpaCheckArgs) -> Result<()> {
    let root = args.root.unwrap_or_else(workspace_root);
    let web_dir = root.join("apps/fabro-web");
    let dist_dir = web_dir.join("dist");
    let asset_dir = root.join("lib/crates/fabro-spa/assets");
    check_spa_asset_budgets(
        &asset_dir,
        args.asset_budget_bytes,
        args.payload_budget_bytes,
    )?;

    if !args.skip_build {
        println!("Running bun run build in apps/fabro-web...");
        run_bun_build(&web_dir)?;
    }

    let staging = TempDir::new(&root, "check")?;
    mirror_dist(&dist_dir, staging.path())?;
    check_spa_asset_budgets(
        staging.path(),
        args.asset_budget_bytes,
        args.payload_budget_bytes,
    )?;
    ensure_assets_match(staging.path(), &asset_dir)?;

    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "dev spa commands report measured budgets directly"
)]
pub(super) fn check_spa_asset_budgets(
    asset_dir: &Path,
    asset_budget_bytes: u64,
    payload_budget_bytes: u64,
) -> Result<()> {
    let report = budget_report(asset_dir)?;

    println!("fabro-spa asset bytes: {}", report.asset_bytes);
    println!(
        "fabro-spa estimated compressed payload bytes: {}",
        report.compressed_payload_bytes
    );

    if report.asset_bytes > asset_budget_bytes {
        bail!(
            "fabro-spa embedded assets exceed budget: {} > {}",
            report.asset_bytes,
            asset_budget_bytes
        );
    }

    if report.compressed_payload_bytes > payload_budget_bytes {
        bail!(
            "fabro-spa compressed payload exceeds budget: {} > {}",
            report.compressed_payload_bytes,
            payload_budget_bytes
        );
    }

    Ok(())
}

struct BudgetReport {
    asset_bytes:              u64,
    compressed_payload_bytes: u64,
}

fn budget_report(asset_dir: &Path) -> Result<BudgetReport> {
    if !asset_dir.is_dir() {
        bail!(
            "fabro-spa assets directory is missing: {}",
            asset_dir.display()
        );
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(asset_dir) {
        let entry = entry.context("walking fabro-spa assets")?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path().to_path_buf();
        if path.extension().and_then(|ext| ext.to_str()) == Some("map") {
            bail!(
                "source map files are not allowed in fabro-spa assets: {}",
                path.display()
            );
        }
        files.push(path);
    }
    files.sort();

    let mut asset_bytes = 0;
    let mut compressed_payload_bytes = 0;
    for file in files {
        asset_bytes += file
            .metadata()
            .with_context(|| format!("reading metadata for {}", file.display()))?
            .len();
        compressed_payload_bytes += gzip_size(&file)?;
    }

    Ok(BudgetReport {
        asset_bytes,
        compressed_payload_bytes,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev spa check intentionally shells out to gzip to match the legacy script"
)]
fn gzip_size(file: &Path) -> Result<u64> {
    let output = Command::new("gzip")
        .args(["-9", "-n", "-c"])
        .arg(file)
        .output()
        .with_context(|| format!("compressing {}", file.display()))?;
    ensure_gzip_success(file, &output)
}

fn ensure_gzip_success(file: &Path, output: &Output) -> Result<u64> {
    if !output.status.success() {
        bail!(
            "gzip failed for {} with {}: {}",
            file.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(output.stdout.len() as u64)
}

fn ensure_assets_match(expected_dir: &Path, actual_dir: &Path) -> Result<()> {
    let expected = directory_snapshot(expected_dir)?;
    let actual = directory_snapshot(actual_dir)?;

    if expected != actual {
        bail!("fabro-spa assets are stale; run `cargo dev spa refresh`");
    }

    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev spa check compares generated asset directories synchronously"
)]
fn directory_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    if !root.is_dir() {
        bail!("fabro-spa assets directory is missing: {}", root.display());
    }

    let mut snapshot = BTreeMap::new();
    for entry in WalkDir::new(root) {
        let entry = entry.context("walking fabro-spa assets")?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", path.display(), root.display()))?
            .to_path_buf();
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        snapshot.insert(relative, bytes);
    }

    Ok(snapshot)
}
