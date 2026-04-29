#![expect(
    clippy::disallowed_methods,
    reason = "CLI `dump` command: sync file I/O for dump outputs"
)]

use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use fabro_store::{RunProjection, StageId};
use fabro_types::RunId;
use fabro_workflow::run_dump::RunDump;
use tokio::task::spawn_blocking;

use crate::args::DumpArgs;
use crate::command_context::CommandContext;
use crate::server_client::Client;
use crate::shared::{absolute_or_current, print_json_pretty};

pub(crate) async fn run(args: &DumpArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.run_id;
    let state = client.get_run_state(&run_id).await?;
    let file_count = export_run(client.as_ref(), &run_id, &state, &args.output).await?;
    if ctx.json_output() {
        print_json_pretty(&serde_json::json!({
            "run_id": run_id,
            "output_dir": absolute_or_current(&args.output),
            "file_count": file_count,
        }))?;
    } else {
        fabro_util::printout!(
            printer,
            "Exported {file_count} files for run {} to {}",
            run_id,
            args.output.display()
        );
    }
    Ok(())
}

async fn export_run(
    client: &Client,
    run_id: &RunId,
    state: &RunProjection,
    output_dir: &Path,
) -> Result<usize> {
    let output_state = inspect_output_dir(output_dir)?;
    let staging_parent = output_parent_dir(output_dir);
    std::fs::create_dir_all(staging_parent)
        .with_context(|| format!("failed to create {}", staging_parent.display()))?;

    let staging_dir = tempfile::Builder::new()
        .prefix(".fabro-dump-")
        .tempdir_in(staging_parent)
        .with_context(|| {
            format!(
                "failed to create staging dir in {}",
                staging_parent.display()
            )
        })?;
    let staging_path = staging_dir.path().to_path_buf();

    let file_count = write_run_dump(client, run_id, state, &staging_path).await?;
    finalize_export(
        output_dir,
        output_state,
        staging_dir,
        &staging_path,
        file_count,
    )
}

async fn write_run_dump(
    client: &Client,
    run_id: &RunId,
    state: &RunProjection,
    output_dir: &Path,
) -> Result<usize> {
    let events = client.list_run_events(run_id, None, None).await?;
    let mut dump = RunDump::from_store_state_and_events(state, &events)?;

    if let Some(log) = client.get_run_logs(run_id).await? {
        dump.add_file_bytes("run.log", log);
    }

    dump.hydrate_referenced_blobs_with_reader(|blob_id| {
        Box::pin(async move { client.read_run_blob(run_id, &blob_id).await })
    })
    .await?;

    for artifact in client.list_run_artifacts(run_id).await? {
        let stage_id: StageId = artifact
            .stage_id
            .parse()
            .with_context(|| format!("server returned invalid stage id {:?}", artifact.stage_id))?;
        let data = client
            .download_stage_artifact(run_id, &stage_id, &artifact.relative_path)
            .await
            .with_context(|| {
                format!(
                    "failed to download artifact {} for stage {}",
                    artifact.relative_path, artifact.stage_id
                )
            })?;
        dump.add_artifact_bytes(&stage_id, &artifact.relative_path, data)?;
    }

    let output_dir = output_dir.to_path_buf();
    spawn_blocking(move || dump.write_to_dir(&output_dir))
        .await
        .map_err(|err| anyhow::anyhow!("run dump write task failed: {err}"))?
}

fn finalize_export(
    output_dir: &Path,
    output_state: OutputDirState,
    staging_dir: tempfile::TempDir,
    staging_path: &Path,
    file_count: usize,
) -> Result<usize> {
    if matches!(output_state, OutputDirState::ExistingEmpty) {
        std::fs::remove_dir(output_dir)
            .with_context(|| format!("failed to replace {}", output_dir.display()))?;
    }
    std::fs::rename(staging_path, output_dir).with_context(|| {
        format!(
            "failed to move staged export {} into {}",
            staging_path.display(),
            output_dir.display()
        )
    })?;
    let _ = staging_dir.keep();

    Ok(file_count)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputDirState {
    Missing,
    ExistingEmpty,
}

fn inspect_output_dir(path: &Path) -> Result<OutputDirState> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(output_dir_error(path));
            }

            let mut entries = std::fs::read_dir(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if entries.next().transpose()?.is_some() {
                return Err(output_dir_error(path));
            }

            Ok(OutputDirState::ExistingEmpty)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(OutputDirState::Missing),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("reading metadata for {}", path.display())))
        }
    }
}

fn output_dir_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "output path {} already exists and is not an empty directory; remove it first or choose a different path",
        path.display()
    )
}

fn output_parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_output_dir_rejects_non_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "x").unwrap();

        let err = inspect_output_dir(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("already exists and is not an empty directory")
        );
    }
}
