use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use clap::Args;
use walkdir::WalkDir;

const DEFAULT_ASSET_BUDGET_BYTES: u64 = 15 * 1024 * 1024;
const DEFAULT_PAYLOAD_BUDGET_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct CheckSpaBudgetsArgs {
    /// Repository root containing lib/crates/fabro-spa/assets.
    #[arg(long, hide = true)]
    root:                 Option<PathBuf>,
    /// Override the raw asset budget.
    #[arg(long, hide = true, default_value_t = DEFAULT_ASSET_BUDGET_BYTES)]
    asset_budget_bytes:   u64,
    /// Override the estimated gzip payload budget.
    #[arg(long, hide = true, default_value_t = DEFAULT_PAYLOAD_BUDGET_BYTES)]
    payload_budget_bytes: u64,
}

#[expect(
    clippy::print_stdout,
    reason = "dev check-spa-budgets command reports measured budgets directly"
)]
pub(crate) fn check_spa_budgets(args: CheckSpaBudgetsArgs) -> Result<()> {
    let root = args.root.unwrap_or_else(workspace_root);
    let asset_dir = root.join("lib/crates/fabro-spa/assets");
    let report = budget_report(&asset_dir)?;

    println!("fabro-spa asset bytes: {}", report.asset_bytes);
    println!(
        "fabro-spa estimated compressed payload bytes: {}",
        report.compressed_payload_bytes
    );

    if report.asset_bytes > args.asset_budget_bytes {
        bail!(
            "fabro-spa committed assets exceed budget: {} > {}",
            report.asset_bytes,
            args.asset_budget_bytes
        );
    }

    if report.compressed_payload_bytes > args.payload_budget_bytes {
        bail!(
            "fabro-spa compressed payload exceeds budget: {} > {}",
            report.compressed_payload_bytes,
            args.payload_budget_bytes
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
    reason = "dev check-spa-budgets intentionally shells out to gzip to match the legacy script"
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

fn workspace_root() -> PathBuf {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    root.pop();
    root.pop();
    root.pop();
    root
}
