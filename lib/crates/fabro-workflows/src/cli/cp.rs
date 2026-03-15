use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::{debug, info};

use crate::cli::runs::{default_runs_base, resolve_run};
use crate::sandbox_record::SandboxRecord;

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

/// Parsed copy direction.
enum CopyDirection {
    /// Download from sandbox to local
    Download {
        run_prefix: String,
        remote_path: String,
        local_path: PathBuf,
    },
    /// Upload from local to sandbox
    Upload {
        local_path: PathBuf,
        run_prefix: String,
        remote_path: String,
    },
}

/// Parse src/dst to determine direction.
///
/// The convention is: `<run-id>:<path>` refers to a sandbox path,
/// and a plain path (no colon) is local. We split on the first colon.
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

/// Split `"run-id:path"` on the first colon.
/// Returns `None` if the string doesn't look like a run-id:path reference.
///
/// We distinguish local paths from run references by checking:
/// - Paths starting with `/`, `./`, or `../` are always local
/// - Otherwise, split on the first colon
pub(crate) fn split_run_path(s: &str) -> Option<(&str, &str)> {
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return None;
    }
    s.split_once(':')
}

/// Reconnect to a sandbox from a saved record.
///
/// Returns a sandbox that can perform file operations.
/// Note: for Docker and Local sandboxes, the container/directory may still
/// need to be alive. For Daytona and Exe, we reconnect via their APIs.
pub async fn reconnect(record: &SandboxRecord) -> Result<Box<dyn fabro_agent::sandbox::Sandbox>> {
    debug!(
        provider = %record.provider,
        identifier = record.identifier.as_deref().unwrap_or(""),
        "Reconnecting to sandbox"
    );

    match record.provider.as_str() {
        "local" => {
            let sandbox = fabro_agent::local_sandbox::LocalSandbox::new(PathBuf::from(
                &record.working_directory,
            ));
            Ok(Box::new(sandbox))
        }
        "docker" => {
            let host_dir = record
                .host_working_directory
                .as_deref()
                .context("Docker sandbox record missing host_working_directory")?;
            let mount_point = record
                .container_mount_point
                .as_deref()
                .unwrap_or("/workspace");

            // Docker uses bind mounts — file operations can go directly through
            // the host filesystem without needing the container running.
            // We create a DockerSandboxConfig with the bind-mount info and use
            // a LocalSandbox pointed at the host directory (since we just need
            // file copy operations, not container exec).
            let config = fabro_agent::docker_sandbox::DockerSandboxConfig {
                host_working_directory: host_dir.to_string(),
                container_mount_point: mount_point.to_string(),
                ..fabro_agent::docker_sandbox::DockerSandboxConfig::default()
            };
            let sandbox = fabro_agent::docker_sandbox::DockerSandbox::new(config)
                .map_err(|e| anyhow::anyhow!("Failed to create Docker sandbox: {e}"))?;
            Ok(Box::new(sandbox))
        }
        "daytona" => {
            let name = record
                .identifier
                .as_deref()
                .context("Daytona sandbox record missing identifier (sandbox name)")?;

            let client = daytona_sdk::Client::new()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create Daytona client: {e}"))?;

            let sdk_sandbox = client.get(name).await.map_err(|e| {
                anyhow::anyhow!("Failed to reconnect to Daytona sandbox '{name}': {e}")
            })?;

            let sandbox =
                crate::daytona_sandbox::DaytonaSandbox::from_existing(client, sdk_sandbox);
            Ok(Box::new(sandbox))
        }
        #[cfg(feature = "exedev")]
        "exe" => {
            let data_host = record
                .data_host
                .as_deref()
                .context("Exe sandbox record missing data_host")?;

            let data_ssh = fabro_exe::OpensshRunner::connect(data_host)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to exe sandbox '{data_host}': {e}")
                })?;

            let sandbox = fabro_exe::ExeSandbox::from_existing(Box::new(data_ssh));
            Ok(Box::new(sandbox))
        }
        "ssh" => {
            let destination = record
                .data_host
                .as_deref()
                .context("SSH sandbox record missing data_host (destination)")?;

            let ssh = fabro_ssh::OpensshRunner::connect(destination, None)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Failed to connect to SSH sandbox '{destination}': {e}")
                })?;

            let config = fabro_ssh::SshConfig {
                destination: destination.to_string(),
                working_directory: record.working_directory.clone(),
                config_file: None,
                preview_url_base: None,
            };
            let sandbox = fabro_ssh::SshSandbox::from_existing(Box::new(ssh), config);
            Ok(Box::new(sandbox))
        }
        other => bail!("Unknown sandbox provider: {other}"),
    }
}

/// Load and reconnect to a sandbox from a run directory.
async fn load_sandbox(
    base: &Path,
    run_prefix: &str,
) -> Result<Box<dyn fabro_agent::sandbox::Sandbox>> {
    let run_dir = resolve_run(base, run_prefix)?.path;
    let sandbox_json = run_dir.join("sandbox.json");
    debug!(path = %sandbox_json.display(), "Loading sandbox record");
    let record = SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    info!(run_id = %run_prefix, provider = %record.provider, "Connecting to sandbox");
    reconnect(&record).await
}

pub async fn cp_command(args: CpArgs) -> Result<()> {
    let direction = parse_direction(&args.src, &args.dst)?;
    let base = default_runs_base();

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
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
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
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            info!(direction = "upload", path = %remote_path, "Copy complete");
        }
    }

    Ok(())
}

/// Recursively download a directory from the sandbox.
async fn download_recursive(
    sandbox: &dyn fabro_agent::sandbox::Sandbox,
    remote_path: &str,
    local_path: &Path,
) -> Result<()> {
    let entries = sandbox
        .list_directory(remote_path, Some(100))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list directory {remote_path}: {e}"))?;

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
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        file_count += 1;
    }
    debug!(count = file_count, "Recursive download complete");
    Ok(())
}

/// Recursively upload a directory to the sandbox.
async fn upload_recursive(
    sandbox: &dyn fabro_agent::sandbox::Sandbox,
    local_path: &Path,
    remote_path: &str,
) -> Result<()> {
    let mut file_count = 0usize;
    let mut stack: Vec<(PathBuf, String)> =
        vec![(local_path.to_path_buf(), remote_path.to_string())];

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
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
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
        let dir = parse_direction("abc123:/some/file.txt", "./local.txt").unwrap();
        match dir {
            CopyDirection::Download {
                run_prefix,
                remote_path,
                local_path,
            } => {
                assert_eq!(run_prefix, "abc123");
                assert_eq!(remote_path, "/some/file.txt");
                assert_eq!(local_path, PathBuf::from("./local.txt"));
            }
            _ => panic!("Expected Download"),
        }
    }

    #[test]
    fn parse_direction_upload() {
        let dir = parse_direction("./local.txt", "abc123:/some/file.txt").unwrap();
        match dir {
            CopyDirection::Upload {
                local_path,
                run_prefix,
                remote_path,
            } => {
                assert_eq!(local_path, PathBuf::from("./local.txt"));
                assert_eq!(run_prefix, "abc123");
                assert_eq!(remote_path, "/some/file.txt");
            }
            _ => panic!("Expected Upload"),
        }
    }

    #[test]
    fn parse_direction_absolute_local_path() {
        let dir = parse_direction("abc123:src/main.rs", "/tmp/main.rs").unwrap();
        match dir {
            CopyDirection::Download {
                run_prefix,
                remote_path,
                local_path,
            } => {
                assert_eq!(run_prefix, "abc123");
                assert_eq!(remote_path, "src/main.rs");
                assert_eq!(local_path, PathBuf::from("/tmp/main.rs"));
            }
            _ => panic!("Expected Download"),
        }
    }

    #[test]
    fn parse_direction_both_sandbox_errors() {
        let result = parse_direction("abc:path", "def:path");
        assert!(result.is_err());
    }

    #[test]
    fn parse_direction_neither_sandbox_errors() {
        let result = parse_direction("./file.txt", "/tmp/file.txt");
        assert!(result.is_err());
    }

    #[test]
    fn parse_direction_relative_upload() {
        let dir = parse_direction("../local.txt", "abc123:remote.txt").unwrap();
        match dir {
            CopyDirection::Upload {
                local_path,
                run_prefix,
                remote_path,
            } => {
                assert_eq!(local_path, PathBuf::from("../local.txt"));
                assert_eq!(run_prefix, "abc123");
                assert_eq!(remote_path, "remote.txt");
            }
            _ => panic!("Expected Upload"),
        }
    }
}
