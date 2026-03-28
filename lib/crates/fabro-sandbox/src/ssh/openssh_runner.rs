use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use openssh::{KnownHosts, SessionBuilder};
use tokio::time;

use super::{SshOutput, SshRunner};
use crate::shell_quote;

/// Real SSH implementation using the `openssh` crate (multiplexed connections).
pub struct OpensshRunner {
    session: openssh::Session,
}

impl OpensshRunner {
    /// Connect to a host via SSH, using the user's SSH agent for authentication.
    /// Commands are executed through a shell (`sh -c`).
    pub async fn connect(destination: &str, config_file: Option<&str>) -> Result<Self, String> {
        let mut builder = SessionBuilder::default();
        builder.known_hosts_check(KnownHosts::Accept);
        if let Some(cfg) = config_file {
            builder.config_file(cfg);
        }
        let session = builder
            .connect(destination)
            .await
            .map_err(|e| format!("SSH connection to {destination} failed: {e}"))?;
        Ok(Self { session })
    }
}

#[async_trait]
impl SshRunner for OpensshRunner {
    async fn run_command(&self, command: &str) -> Result<SshOutput, String> {
        let output = self
            .session
            .shell(command)
            .output()
            .await
            .map_err(|e| format!("SSH command failed: {e}"))?;

        let exit_code = output.status.code().unwrap_or(-1);
        Ok(SshOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code,
        })
    }

    async fn run_command_with_timeout(
        &self,
        command: &str,
        timeout: std::time::Duration,
    ) -> Result<SshOutput, String> {
        let mut child = self.session.shell(command);
        let fut = child.output();

        match time::timeout(timeout, fut).await {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                Ok(SshOutput {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit_code,
                })
            }
            Ok(Err(e)) => Err(format!("SSH command failed: {e}")),
            Err(_) => Err("Command timed out".to_string()),
        }
    }

    async fn upload_file(&self, path: &str, content: &[u8]) -> Result<(), String> {
        use base64::Engine;
        let encoded = STANDARD.encode(content);
        let cmd = format!("echo '{}' | base64 -d > {}", encoded, shell_quote(path),);
        let output = self
            .session
            .shell(&cmd)
            .output()
            .await
            .map_err(|e| format!("SSH upload failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Upload to {path} failed: {stderr}"));
        }
        Ok(())
    }

    async fn download_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let cmd = format!("cat {}", shell_quote(path));
        let output = self
            .session
            .shell(&cmd)
            .output()
            .await
            .map_err(|e| format!("SSH download failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Download of {path} failed: {stderr}"));
        }
        Ok(output.stdout)
    }
}
