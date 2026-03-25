use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::{debug, info};

use super::shared::split_run_path;

#[derive(Args)]
pub struct CpArgs {
    /// Source: <run-id>:<path> or local path
    pub src: String,
    /// Destination: <run-id>:<path> or local path
    pub dst: String,
    /// Recurse into directories
    #[arg(short, long)]
    pub recursive: bool,
}

enum CopyDirection {
    Download {
        run_prefix: String,
        remote_path: String,
        local_path: PathBuf,
    },
    Upload {
        local_path: PathBuf,
        run_prefix: String,
        remote_path: String,
    },
}

pub async fn cp_command(args: CpArgs) -> Result<()> {
    let direction = parse_direction(&args.src, &args.dst)?;
    let base = fabro_workflows::run_lookup::default_runs_base();

    match direction {
        CopyDirection::Download {
            run_prefix,
            remote_path,
            local_path,
        } => {
            let sandbox = load_sandbox(&base, &run_prefix).await?;

            if args.recursive {
                download_recursive(&*sandbox, &remote_path, &local_path).await?;
            } else {
                debug!(path = %remote_path, "Downloading file from sandbox");
                sandbox
                    .download_file_to_local(&remote_path, &local_path)
                    .await
                    .map_err(|err| anyhow::anyhow!("{err}"))?;
            }
            info!(direction = "download", path = %remote_path, "Copy complete");
        }
        CopyDirection::Upload {
            local_path,
            run_prefix,
            remote_path,
        } => {
            let sandbox = load_sandbox(&base, &run_prefix).await?;

            if args.recursive {
                upload_recursive(&*sandbox, &local_path, &remote_path).await?;
            } else {
                debug!(path = %remote_path, "Uploading file to sandbox");
                sandbox
                    .upload_file_from_local(&local_path, &remote_path)
                    .await
                    .map_err(|err| anyhow::anyhow!("{err}"))?;
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
            run_prefix: run_prefix.to_string(),
            remote_path: remote_path.to_string(),
            local_path: PathBuf::from(dst),
        }),
        (None, Some((run_prefix, remote_path))) => Ok(CopyDirection::Upload {
            local_path: PathBuf::from(src),
            run_prefix: run_prefix.to_string(),
            remote_path: remote_path.to_string(),
        }),
        (Some(_), Some(_)) => {
            bail!("Cannot copy between two sandboxes; one argument must be a local path")
        }
        (None, None) => bail!("One argument must contain a run-id prefix (e.g. <run-id>:<path>)"),
    }
}

async fn load_sandbox(
    base: &Path,
    run_prefix: &str,
) -> Result<Box<dyn fabro_agent::sandbox::Sandbox>> {
    let run_dir = fabro_workflows::run_lookup::resolve_run(base, run_prefix)?.path;
    let sandbox_json = run_dir.join("sandbox.json");
    debug!(path = %sandbox_json.display(), "Loading sandbox record");
    let record = fabro_workflows::records::SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    info!(run_id = %run_prefix, provider = %record.provider, "Connecting to sandbox");
    fabro_workflows::sandbox_reconnect::reconnect(&record).await
}

async fn download_recursive(
    sandbox: &dyn fabro_agent::sandbox::Sandbox,
    remote_path: &str,
    local_path: &Path,
) -> Result<()> {
    let entries = sandbox
        .list_directory(remote_path, Some(100))
        .await
        .map_err(|err| anyhow::anyhow!("Failed to list directory {remote_path}: {err}"))?;

    let mut file_count = 0usize;
    for entry in &entries {
        if entry.is_dir {
            continue;
        }
        let remote_file = format!("{remote_path}/{}", entry.name);
        let local_file = local_path.join(&entry.name);
        if let Some(parent) = local_file.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        debug!(path = %remote_file, "Downloading file from sandbox");
        sandbox
            .download_file_to_local(&remote_file, &local_file)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        file_count += 1;
    }
    debug!(count = file_count, "Recursive download complete");
    Ok(())
}

async fn upload_recursive(
    sandbox: &dyn fabro_agent::sandbox::Sandbox,
    local_path: &Path,
    remote_path: &str,
) -> Result<()> {
    let mut file_count = 0usize;
    let mut stack = vec![(local_path.to_path_buf(), remote_path.to_string())];

    while let Some((dir_path, dir_remote)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir_path)
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
                sandbox
                    .upload_file_from_local(&entry_path, &remote_file)
                    .await
                    .map_err(|err| anyhow::anyhow!("{err}"))?;
                file_count += 1;
            }
        }
    }
    debug!(count = file_count, "Recursive upload complete");
    Ok(())
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
    fn split_run_path_ignores_local_paths() {
        assert_eq!(split_run_path("/tmp/file"), None);
        assert_eq!(split_run_path("./file"), None);
        assert_eq!(split_run_path("../file"), None);
    }
}
