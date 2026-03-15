use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;

use crate::asset_snapshot::AssetCollectionSummary;
use crate::cli::cp::split_run_path;
use crate::cli::runs::{default_runs_base, format_size, resolve_run};

/// An individual asset file discovered from a run's asset manifests.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AssetEntry {
    pub node_slug: String,
    pub retry: u32,
    pub relative_path: String,
    #[serde(serialize_with = "serialize_path")]
    pub absolute_path: PathBuf,
    pub size: u64,
}

fn serialize_path<S: serde::Serializer>(path: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&path.display().to_string())
}

/// Walk `{run_dir}/artifacts/assets/*/retry_*/manifest.json`, stat each file, return entries.
pub fn scan_assets(run_dir: &Path, node_filter: Option<&str>) -> Result<Vec<AssetEntry>> {
    let assets_dir = run_dir.join("artifacts/assets");
    let nodes = match std::fs::read_dir(&assets_dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(Vec::new()),
    };

    let mut entries = Vec::new();
    for node_entry in nodes.flatten() {
        if !node_entry.path().is_dir() {
            continue;
        }
        let node_slug = node_entry.file_name().to_string_lossy().into_owned();

        if let Some(filter) = node_filter {
            if node_slug != filter {
                continue;
            }
        }

        let Ok(retries) = std::fs::read_dir(node_entry.path()) else {
            continue;
        };
        for retry_entry in retries.flatten() {
            let retry_dir = retry_entry.path();
            let dir_name = retry_entry.file_name().to_string_lossy().into_owned();
            let retry: u32 = dir_name
                .strip_prefix("retry_")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);

            let manifest = retry_dir.join("manifest.json");
            let Ok(contents) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(summary) = serde_json::from_str::<AssetCollectionSummary>(&contents) else {
                continue;
            };

            for relative_path in &summary.copied_paths {
                let absolute_path = retry_dir.join(relative_path);
                let size = std::fs::metadata(&absolute_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                entries.push(AssetEntry {
                    node_slug: node_slug.clone(),
                    retry,
                    relative_path: relative_path.clone(),
                    absolute_path,
                    size,
                });
            }
        }
    }
    Ok(entries)
}

#[derive(Args)]
pub struct AssetListArgs {
    /// Run ID (or prefix)
    pub run_id: String,

    /// Filter to assets from a specific node
    #[arg(long)]
    pub node: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct AssetCpArgs {
    /// Source: RUN_ID (all assets) or RUN_ID:path (specific asset)
    pub source: String,

    /// Destination directory (defaults to current directory)
    #[arg(default_value = ".")]
    pub dest: PathBuf,

    /// Filter to assets from a specific node
    #[arg(long)]
    pub node: Option<String>,

    /// Preserve {node_slug}/retry_{N}/ directory structure
    #[arg(long)]
    pub tree: bool,
}

/// Parse `source` into (run_id, optional_asset_path) using the same colon-split logic as `cp`.
fn parse_source(s: &str) -> (&str, Option<&str>) {
    match split_run_path(s) {
        Some((run_id, path)) => (run_id, Some(path)),
        None => (s, None),
    }
}

pub fn list_command(args: &AssetListArgs) -> Result<()> {
    let base = default_runs_base();
    let run_info = resolve_run(&base, &args.run_id)?;
    let entries = scan_assets(&run_info.path, args.node.as_deref())?;

    if args.json {
        let json = serde_json::to_string_pretty(&entries)?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        println!("No assets found for this run.");
        return Ok(());
    }

    // Compute column widths
    let node_width = entries
        .iter()
        .map(|e| e.node_slug.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let retry_width = 5; // "RETRY"
    let size_width = entries
        .iter()
        .map(|e| format_size(e.size).len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!(
        "{:<node_width$}  {:>retry_width$}  {:>size_width$}  PATH",
        "NODE", "RETRY", "SIZE"
    );
    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    for entry in &entries {
        println!(
            "{:<node_width$}  {:>retry_width$}  {:>size_width$}  {}",
            entry.node_slug,
            entry.retry,
            format_size(entry.size),
            entry.relative_path
        );
    }
    println!();
    println!(
        "{} asset(s), {} total",
        entries.len(),
        format_size(total_size)
    );

    Ok(())
}

pub fn cp_command(args: &AssetCpArgs) -> Result<()> {
    let base = default_runs_base();
    let (run_id, asset_path) = parse_source(&args.source);
    let run_info = resolve_run(&base, run_id)?;
    let entries = scan_assets(&run_info.path, args.node.as_deref())?;

    if entries.is_empty() {
        bail!("No assets found for this run");
    }

    std::fs::create_dir_all(&args.dest)
        .with_context(|| format!("Failed to create destination: {}", args.dest.display()))?;

    if let Some(path) = asset_path {
        // Copy a specific asset
        let matching: Vec<_> = entries.iter().filter(|e| e.relative_path == path).collect();
        if matching.is_empty() {
            bail!("No asset matching path '{path}' found in this run");
        }
        if matching.len() > 1 && args.node.is_none() {
            let nodes: Vec<_> = matching.iter().map(|e| e.node_slug.as_str()).collect();
            bail!(
                "Path '{path}' exists in multiple nodes: {}. Use --node to disambiguate.",
                nodes.join(", ")
            );
        }
        let entry = matching[0];
        let dest_file = args.dest.join(
            Path::new(&entry.relative_path)
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new(&entry.relative_path)),
        );
        if let Some(parent) = dest_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&entry.absolute_path, &dest_file).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                entry.absolute_path.display(),
                dest_file.display()
            )
        })?;
        println!("Copied {} to {}", entry.relative_path, dest_file.display());
    } else {
        // Copy all assets
        if args.tree {
            // Preserve directory structure: {node_slug}/retry_{N}/...
            for entry in &entries {
                let rel = PathBuf::from(&entry.node_slug)
                    .join(format!("retry_{}", entry.retry))
                    .join(&entry.relative_path);
                let dest_file = args.dest.join(&rel);
                if let Some(parent) = dest_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&entry.absolute_path, &dest_file).with_context(|| {
                    format!(
                        "Failed to copy {} to {}",
                        entry.absolute_path.display(),
                        dest_file.display()
                    )
                })?;
            }
        } else {
            // Flat mode: build filename map and check for collisions
            let mut by_filename: Vec<(String, &AssetEntry)> = Vec::with_capacity(entries.len());
            for entry in &entries {
                let filename = Path::new(&entry.relative_path)
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new(&entry.relative_path))
                    .to_string_lossy()
                    .into_owned();
                if let Some((_, existing)) = by_filename.iter().find(|(f, _)| f == &filename) {
                    bail!(
                        "Filename collision: '{}' exists in both node '{}' and '{}'. \
                         Use --tree to preserve directory structure, or --node to filter.",
                        filename,
                        existing.node_slug,
                        entry.node_slug
                    );
                }
                by_filename.push((filename, entry));
            }

            for (filename, entry) in &by_filename {
                let dest_file = args.dest.join(filename);
                if let Some(parent) = dest_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&entry.absolute_path, &dest_file).with_context(|| {
                    format!(
                        "Failed to copy {} to {}",
                        entry.absolute_path.display(),
                        dest_file.display()
                    )
                })?;
            }
        }
        println!(
            "Copied {} asset(s) to {}",
            entries.len(),
            args.dest.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_bare_run_id() {
        let (id, path) = parse_source("01ABC");
        assert_eq!(id, "01ABC");
        assert_eq!(path, None);
    }

    #[test]
    fn parse_source_with_path() {
        let (id, path) = parse_source("01ABC:test-results/report.xml");
        assert_eq!(id, "01ABC");
        assert_eq!(path, Some("test-results/report.xml"));
    }

    #[test]
    fn parse_source_local_absolute_path() {
        let (id, path) = parse_source("/tmp/foo");
        assert_eq!(id, "/tmp/foo");
        assert_eq!(path, None);
    }

    #[test]
    fn parse_source_local_relative_path() {
        let (id, path) = parse_source("./foo");
        assert_eq!(id, "./foo");
        assert_eq!(path, None);
    }
}