use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;

use crate::args::ArtifactCpArgs;
use crate::server_client::ServerStoreClient;
use crate::shared::{print_json_pretty, split_run_path};

pub(super) async fn cp_command(
    args: &ArtifactCpArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let (run_id_selector, asset_path) = parse_source(&args.source);
    let (run_id, client, entries) = super::resolve_artifacts(
        &args.server,
        run_id_selector,
        args.node.as_deref(),
        args.retry,
        cli,
        cli_layer,
        printer,
    )
    .await?;

    if entries.is_empty() {
        bail!("No artifacts found for this run");
    }

    std::fs::create_dir_all(&args.dest)
        .with_context(|| format!("Failed to create destination: {}", args.dest.display()))?;

    if let Some(path) = asset_path {
        let matching: Vec<_> = entries
            .iter()
            .filter(|entry| entry.relative_path == path)
            .collect();
        if matching.is_empty() {
            bail!("No artifact matching path '{path}' found in this run");
        }
        if matching.len() > 1 {
            let candidates: Vec<_> = matching
                .iter()
                .map(|entry| format_candidate(entry))
                .collect();
            bail!(
                "Path '{path}' matches multiple artifacts: {}. Use --node and/or --retry to disambiguate.",
                candidates.join(", ")
            );
        }

        let entry = matching[0];
        let dest_file = args.dest.join(
            Path::new(&entry.relative_path)
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new(&entry.relative_path)),
        );
        write_artifact_file(&client, &run_id, entry, &dest_file).await?;
        if cli.output.format == OutputFormat::Json {
            print_json_pretty(&serde_json::json!({
                "copied": [{
                    "relative_path": entry.relative_path,
                    "destination": dest_file.display().to_string(),
                }],
            }))?;
        } else {
            fabro_util::printout!(
                printer,
                "Copied {} to {}",
                entry.relative_path,
                dest_file.display()
            );
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
            write_artifact_file(&client, &run_id, entry, &dest_file).await?;
            copied.push(serde_json::json!({
                "relative_path": entry.relative_path,
                "destination": dest_file.display().to_string(),
            }));
        }
    } else {
        let mut by_filename: Vec<(String, &super::ArtifactEntry)> =
            Vec::with_capacity(entries.len());
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
            write_artifact_file(&client, &run_id, entry, &dest_file).await?;
            copied.push(serde_json::json!({
                "relative_path": entry.relative_path,
                "destination": dest_file.display().to_string(),
            }));
        }
    }

    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&serde_json::json!({ "copied": copied }))?;
    } else {
        fabro_util::printout!(
            printer,
            "Copied {} artifact(s) to {}",
            entries.len(),
            args.dest.display()
        );
    }
    Ok(())
}

async fn write_artifact_file(
    client: &ServerStoreClient,
    run_id: &fabro_types::RunId,
    entry: &super::ArtifactEntry,
    dest_file: &Path,
) -> Result<()> {
    if let Some(parent) = dest_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = client
        .download_stage_artifact(run_id, &entry.stage_id, &entry.relative_path)
        .await?;
    std::fs::write(dest_file, bytes)
        .with_context(|| format!("Failed to write {}", dest_file.display()))?;
    Ok(())
}

fn parse_source(source: &str) -> (&str, Option<&str>) {
    match split_run_path(source) {
        Some((run_id, path)) => (run_id, Some(path)),
        None => (source, None),
    }
}

fn format_candidate(entry: &super::ArtifactEntry) -> String {
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
        let entry = super::super::ArtifactEntry {
            node_slug: "retry_assets".to_string(),
            retry: 2,
            stage_id: fabro_types::StageId::new("retry_assets", 2),
            relative_path: "assets/retry/report.txt".to_string(),
            size: 6,
        };

        assert_eq!(format_candidate(&entry), "retry_assets:retry_2");
    }
}
