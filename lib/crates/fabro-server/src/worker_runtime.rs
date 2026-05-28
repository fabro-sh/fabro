use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;

use anyhow::{Context, Result};
use async_trait::async_trait;
use fabro_static::EnvVars;
use fabro_types::RunId;
use futures_util::future::BoxFuture;
use tokio::io::AsyncRead;
use tokio::process::{ChildStderr, Command};

use crate::spawn_env::apply_worker_env;

#[async_trait]
pub(crate) trait WorkerRuntime: Send + Sync {
    async fn start(&self, spec: WorkerLaunchSpec) -> Result<StartedWorker>;
    async fn request_stop(&self, worker_ref: &WorkerRef) -> Result<()>;
    async fn force_stop(&self, worker_ref: &WorkerRef) -> Result<()>;
    async fn is_alive(&self, worker_ref: &WorkerRef) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerRef {
    Local {
        pid:              u32,
        process_group_id: Option<u32>,
    },
}

pub(crate) struct WorkerLaunchSpec {
    pub(crate) executable:             PathBuf,
    pub(crate) server_target:          String,
    pub(crate) storage_dir:            PathBuf,
    pub(crate) run_dir:                PathBuf,
    pub(crate) run_id:                 RunId,
    pub(crate) mode:                   &'static str,
    pub(crate) worker_token:           String,
    pub(crate) stdout:                 WorkerStdout,
    pub(crate) fabro_log:              Option<String>,
    pub(crate) fabro_log_destination:  &'static str,
    pub(crate) active_config_path:     PathBuf,
    pub(crate) github_app_private_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerStdout {
    Inherit,
    Null,
}

pub(crate) struct StartedWorker {
    pub(crate) worker_ref: WorkerRef,
    pub(crate) stderr:     Option<Pin<Box<dyn AsyncRead + Send + 'static>>>,
    pub(crate) wait:       BoxFuture<'static, Result<WorkerExit>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerExit {
    pub(crate) success: bool,
    pub(crate) detail:  String,
}

#[derive(Default)]
pub(crate) struct LocalWorkerRuntime;

impl LocalWorkerRuntime {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn command_for_spec(spec: &WorkerLaunchSpec) -> Command {
        let worker_stdout = match spec.stdout {
            WorkerStdout::Inherit => Stdio::inherit(),
            WorkerStdout::Null => Stdio::null(),
        };

        let mut cmd = Command::new(&spec.executable);
        cmd.arg("__run-worker")
            .arg("--server")
            .arg(&spec.server_target)
            .arg("--storage-dir")
            .arg(&spec.storage_dir)
            .arg("--run-dir")
            .arg(&spec.run_dir)
            .arg("--run-id")
            .arg(spec.run_id.to_string())
            .arg("--mode")
            .arg(spec.mode)
            .stdin(Stdio::null())
            .stdout(worker_stdout)
            .stderr(Stdio::piped());

        apply_worker_env(&mut cmd);
        if let Some(level) = spec.fabro_log.as_deref() {
            cmd.env(EnvVars::FABRO_LOG, level);
        }
        cmd.env(EnvVars::FABRO_LOG_DESTINATION, spec.fabro_log_destination);
        cmd.env(EnvVars::FABRO_CONFIG, &spec.active_config_path);
        cmd.env_remove(EnvVars::FABRO_WORKER_TOKEN);
        cmd.env(EnvVars::FABRO_WORKER_TOKEN, &spec.worker_token);
        if let Some(pem) = spec.github_app_private_key.as_deref() {
            cmd.env(EnvVars::GITHUB_APP_PRIVATE_KEY, pem);
        }

        #[cfg(unix)]
        fabro_proc::pre_exec_setpgid(cmd.as_std_mut());

        cmd
    }
}

#[async_trait]
impl WorkerRuntime for LocalWorkerRuntime {
    async fn start(&self, spec: WorkerLaunchSpec) -> Result<StartedWorker> {
        let mut child = Self::command_for_spec(&spec)
            .spawn()
            .context("spawning run worker process")?;

        let pid = child.id().context("worker process did not report a PID")?;
        let stderr = child.stderr.take().map(box_stderr);
        let wait: BoxFuture<'static, Result<WorkerExit>> = Box::pin(async move {
            let status = child.wait().await.context("worker wait failed")?;
            Ok(WorkerExit {
                success: status.success(),
                detail:  status.to_string(),
            })
        });

        Ok(StartedWorker {
            worker_ref: WorkerRef::Local {
                pid,
                process_group_id: Some(pid),
            },
            stderr,
            wait,
        })
    }

    async fn request_stop(&self, worker_ref: &WorkerRef) -> Result<()> {
        match worker_ref {
            WorkerRef::Local { pid, .. } => {
                #[cfg(unix)]
                fabro_proc::sigterm(*pid);

                #[cfg(not(unix))]
                let _ = pid;
            }
        }
        Ok(())
    }

    async fn force_stop(&self, worker_ref: &WorkerRef) -> Result<()> {
        match worker_ref {
            WorkerRef::Local {
                pid,
                process_group_id,
            } => {
                #[cfg(unix)]
                fabro_proc::sigkill_process_group(process_group_id.unwrap_or(*pid));

                #[cfg(not(unix))]
                let _ = (pid, process_group_id);
            }
        }
        Ok(())
    }

    async fn is_alive(&self, worker_ref: &WorkerRef) -> bool {
        match worker_ref {
            WorkerRef::Local {
                pid,
                process_group_id,
            } => {
                #[cfg(unix)]
                {
                    fabro_proc::process_group_alive(process_group_id.unwrap_or(*pid))
                }
                #[cfg(not(unix))]
                {
                    let _ = process_group_id;
                    fabro_proc::process_running(*pid)
                }
            }
        }
    }
}

fn box_stderr(stderr: ChildStderr) -> Pin<Box<dyn AsyncRead + Send + 'static>> {
    Box::pin(stderr)
}
