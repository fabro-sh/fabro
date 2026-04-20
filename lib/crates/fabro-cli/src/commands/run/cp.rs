use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use tokio::fs;
use tracing::{debug, info};

use crate::args::{CpArgs, ServerTargetArgs};
use crate::command_context::CommandContext;
use crate::server_client::ServerStoreClient;
use crate::shared::{print_json_pretty, split_run_path};

#[derive(Debug)]
enum CopyDirection {
    Download {
        run_prefix:  String,
        remote_path: String,
        local_path:  PathBuf,
    },
    Upload {
        local_path:  PathBuf,
        run_prefix:  String,
        remote_path: String,
    },
}

pub(crate) async fn cp_command(
    args: CpArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let direction = parse_direction(&args.src, &args.dst)?;

    match direction {
        CopyDirection::Download {
            run_prefix,
            remote_path,
            local_path,
        } => {
            let (client, run_id) =
                resolve_client_and_run_id(&args.server, &run_prefix, cli, cli_layer, printer)
                    .await?;

            let file_count = if args.recursive {
                Some(download_recursive(&client, &run_id, &remote_path, &local_path).await?)
            } else {
                debug!(path = %remote_path, "Downloading file from sandbox");
                write_sandbox_file(&client, &run_id, &remote_path, &local_path).await?;
                None
            };

            if cli.output.format == OutputFormat::Json {
                let mut value = serde_json::json!({
                    "direction": "download",
                    "recursive": args.recursive,
                    "remote_path": remote_path,
                    "local_path": local_path,
                });
                if let Some(count) = file_count {
                    value["file_count"] = count.into();
                }
                print_json_pretty(&value)?;
            }

            info!(direction = "download", path = %remote_path, "Copy complete");
        }
        CopyDirection::Upload {
            local_path,
            run_prefix,
            remote_path,
        } => {
            let (client, run_id) =
                resolve_client_and_run_id(&args.server, &run_prefix, cli, cli_layer, printer)
                    .await?;

            let file_count = if args.recursive {
                Some(upload_recursive(&client, &run_id, &local_path, &remote_path).await?)
            } else {
                debug!(path = %remote_path, "Uploading file to sandbox");
                upload_sandbox_file(&client, &run_id, &local_path, &remote_path).await?;
                None
            };

            if cli.output.format == OutputFormat::Json {
                let mut value = serde_json::json!({
                    "direction": "upload",
                    "recursive": args.recursive,
                    "remote_path": remote_path,
                    "local_path": local_path,
                });
                if let Some(count) = file_count {
                    value["file_count"] = count.into();
                }
                print_json_pretty(&value)?;
            }
            info!(direction = "upload", path = %remote_path, "Copy complete");
        }
    }

    Ok(())
}

fn parse_direction(src: &str, dst: &str) -> Result<CopyDirection> {
    let src_parts = split_run_path(src);
    let dst_parts = split_run_path(dst);

    match (src_parts, dst_parts) {
        (Some((run_prefix, remote_path)), None) => Ok(CopyDirection::Download {
            run_prefix:  run_prefix.to_string(),
            remote_path: remote_path.to_string(),
            local_path:  PathBuf::from(dst),
        }),
        (None, Some((run_prefix, remote_path))) => Ok(CopyDirection::Upload {
            local_path:  PathBuf::from(src),
            run_prefix:  run_prefix.to_string(),
            remote_path: remote_path.to_string(),
        }),
        (Some(_), Some(_)) => {
            bail!("Cannot copy between two sandboxes; one argument must be a local path")
        }
        (None, None) => bail!("One argument must contain a run-id prefix (e.g. <run-id>:<path>)"),
    }
}

async fn resolve_client_and_run_id(
    server: &ServerTargetArgs,
    run_prefix: &str,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<(ServerStoreClient, fabro_types::RunId)> {
    let ctx = CommandContext::for_target(server, printer, cli.clone(), cli_layer)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(run_prefix).await?.run_id;
    Ok((client.clone_for_reuse(), run_id))
}

async fn write_sandbox_file(
    client: &ServerStoreClient,
    run_id: &fabro_types::RunId,
    remote_path: &str,
    local_path: &Path,
) -> Result<()> {
    if let Some(parent) = local_path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    let bytes = client.get_sandbox_file(run_id, remote_path).await?;
    fs::write(local_path, bytes)
        .await
        .with_context(|| format!("Failed to write {}", local_path.display()))?;
    Ok(())
}

async fn upload_sandbox_file(
    client: &ServerStoreClient,
    run_id: &fabro_types::RunId,
    local_path: &Path,
    remote_path: &str,
) -> Result<()> {
    let bytes = fs::read(local_path)
        .await
        .with_context(|| format!("Failed to read {}", local_path.display()))?;
    client.put_sandbox_file(run_id, remote_path, bytes).await
}

async fn download_recursive(
    client: &ServerStoreClient,
    run_id: &fabro_types::RunId,
    remote_path: &str,
    local_path: &Path,
) -> Result<usize> {
    let entries = client
        .list_sandbox_files(run_id, remote_path, Some(100))
        .await?;

    let mut file_count = 0usize;
    for entry in &entries {
        if entry.is_dir {
            continue;
        }
        let remote_file = format!("{remote_path}/{}", entry.name);
        let local_file = local_path.join(&entry.name);
        debug!(path = %remote_file, "Downloading file from sandbox");
        write_sandbox_file(client, run_id, &remote_file, &local_file).await?;
        file_count += 1;
    }
    debug!(count = file_count, "Recursive download complete");
    Ok(file_count)
}

async fn upload_recursive(
    client: &ServerStoreClient,
    run_id: &fabro_types::RunId,
    local_path: &Path,
    remote_path: &str,
) -> Result<usize> {
    let mut file_count = 0usize;
    let mut stack = vec![(local_path.to_path_buf(), remote_path.to_string())];

    while let Some((dir_path, dir_remote)) = stack.pop() {
        let mut entries = fs::read_dir(&dir_path)
            .await
            .with_context(|| format!("Failed to read directory {}", dir_path.display()))?;

        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            let remote_file = format!("{dir_remote}/{file_name}");

            if entry.file_type().await?.is_dir() {
                stack.push((entry_path, remote_file));
            } else {
                debug!(path = %remote_file, "Uploading file to sandbox");
                upload_sandbox_file(client, run_id, &entry_path, &remote_file).await?;
                file_count += 1;
            }
        }
    }
    debug!(count = file_count, "Recursive upload complete");
    Ok(file_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direction_download() {
        let direction = parse_direction("abc123:/some/file.txt", "./local.txt").unwrap();
        match direction {
            CopyDirection::Download {
                run_prefix,
                remote_path,
                local_path,
            } => {
                assert_eq!(run_prefix, "abc123");
                assert_eq!(remote_path, "/some/file.txt");
                assert_eq!(local_path, PathBuf::from("./local.txt"));
            }
            CopyDirection::Upload { .. } => panic!("expected download"),
        }
    }

    #[test]
    fn parse_direction_upload() {
        let direction = parse_direction("./local.txt", "abc123:/remote.txt").unwrap();
        match direction {
            CopyDirection::Upload {
                local_path,
                run_prefix,
                remote_path,
            } => {
                assert_eq!(local_path, PathBuf::from("./local.txt"));
                assert_eq!(run_prefix, "abc123");
                assert_eq!(remote_path, "/remote.txt");
            }
            CopyDirection::Download { .. } => panic!("expected upload"),
        }
    }

    #[test]
    fn parse_direction_rejects_sandbox_to_sandbox_copy() {
        let err = parse_direction("abc123:/in.txt", "def456:/out.txt").unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot copy between two sandboxes")
        );
    }
}
