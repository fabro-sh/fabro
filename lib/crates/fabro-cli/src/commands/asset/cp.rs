use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fabro_store::RuntimeState;
use fabro_workflow::assets::{AssetEntry, scan_assets};
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};

use crate::args::{AssetCpArgs, GlobalArgs};
use crate::shared::{print_json_pretty, split_run_path};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(super) async fn cp_command(args: &AssetCpArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let (run_id, asset_path) = parse_source(&args.source);
    let run = resolve_run_combined(store.as_ref(), &base, run_id).await?;
    let runtime_state = RuntimeState::new(&run.path);
    let entries = scan_assets(
        &runtime_state.assets_dir(),
        args.node.as_deref(),
        args.retry,
    )?;

    if entries.is_empty() {
        bail!("No assets found for this run");
    }

    std::fs::create_dir_all(&args.dest)
        .with_context(|| format!("Failed to create destination: {}", args.dest.display()))?;

    if let Some(path) = asset_path {
        let matching: Vec<_> = entries
            .iter()
            .filter(|entry| entry.relative_path == path)
            .collect();
        if matching.is_empty() {
            bail!("No asset matching path '{path}' found in this run");
        }
        if matching.len() > 1 {
            let candidates: Vec<_> = matching
                .iter()
                .map(|entry| format!("{}:retry_{}", entry.node_slug, entry.retry))
                .collect();
            bail!(
                "Path '{path}' matches multiple assets: {}. Use --node and/or --retry to disambiguate.",
                candidates.join(", ")
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
        if globals.json {
            print_json_pretty(&serde_json::json!({
                "copied": [{
                    "relative_path": entry.relative_path,
                    "destination": dest_file.display().to_string(),
                }],
            }))?;
        } else {
            println!("Copied {} to {}", entry.relative_path, dest_file.display());
        }
        return Ok(());
    }

    let mut copied = Vec::new();
    if args.tree {
        for entry in &entries {
            let relative_dest = PathBuf::from(&entry.node_slug)
                .join(format!("retry_{}", entry.retry))
                .join(&entry.relative_path);
            let dest_file = args.dest.join(relative_dest);
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
            copied.push(serde_json::json!({
                "relative_path": entry.relative_path,
                "destination": dest_file.display().to_string(),
            }));
        }
    } else {
        let mut by_filename: Vec<(String, &AssetEntry)> = Vec::with_capacity(entries.len());
        for entry in &entries {
            let filename = Path::new(&entry.relative_path)
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new(&entry.relative_path))
                .to_string_lossy()
                .into_owned();
            if let Some((_, existing)) = by_filename.iter().find(|(name, _)| name == &filename) {
                bail!(
                    "Filename collision: '{}' exists in both {} and {}. Use --tree to preserve directory structure, or --node and/or --retry to filter.",
                    filename,
                    format_candidate(existing),
                    format_candidate(entry)
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
            copied.push(serde_json::json!({
                "relative_path": entry.relative_path,
                "destination": dest_file.display().to_string(),
            }));
        }
    }

    if globals.json {
        print_json_pretty(&serde_json::json!({ "copied": copied }))?;
    } else {
        println!(
            "Copied {} asset(s) to {}",
            entries.len(),
            args.dest.display()
        );
    }
    Ok(())
}

fn parse_source(source: &str) -> (&str, Option<&str>) {
    match split_run_path(source) {
        Some((run_id, path)) => (run_id, Some(path)),
        None => (source, None),
    }
}

fn format_candidate(entry: &AssetEntry) -> String {
    format!("{}:retry_{}", entry.node_slug, entry.retry)
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

    #[test]
    fn format_candidate_includes_retry() {
        let entry = AssetEntry {
            node_slug: "retry_assets".to_string(),
            retry: 2,
            relative_path: "assets/retry/report.txt".to_string(),
            absolute_path: PathBuf::from("/tmp/report.txt"),
            size: 6,
        };

        assert_eq!(format_candidate(&entry), "retry_assets:retry_2");
    }
}
