use std::collections::HashMap;
use std::io::Result as IoResult;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::util::internal_error;
use agent_client_protocol::{
    Agent, Client, ConnectTo, Error as ProtocolError, Lines, Result as AcpProtocolResult,
};
use fabro_sandbox::{
    Error as SandboxError, Result as SandboxResult, Sandbox, StderrCollector, StdioProcessHandle,
};
use futures::io::BufReader;
use futures::sink::unfold;
use futures::{AsyncBufReadExt, AsyncWriteExt, Stream};
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::command::AcpCommand;

#[derive(Clone)]
pub(crate) struct TransportState {
    handle:        Arc<TokioMutex<Option<StdioProcessHandle>>>,
    stderr:        Arc<TokioMutex<Option<StderrCollector>>>,
    startup_error: Arc<TokioMutex<Option<SandboxError>>>,
}

impl TransportState {
    pub(crate) fn new() -> Self {
        Self {
            handle:        Arc::new(TokioMutex::new(None)),
            stderr:        Arc::new(TokioMutex::new(None)),
            startup_error: Arc::new(TokioMutex::new(None)),
        }
    }

    async fn set_process(&self, handle: StdioProcessHandle, stderr: StderrCollector) {
        *self.handle.lock().await = Some(handle);
        *self.stderr.lock().await = Some(stderr);
    }

    async fn set_startup_error(&self, error: SandboxError) {
        *self.startup_error.lock().await = Some(error);
    }

    pub(crate) async fn take_startup_error(&self) -> Option<SandboxError> {
        self.startup_error.lock().await.take()
    }

    pub(crate) async fn terminate(&self) -> SandboxResult<()> {
        if let Some(handle) = self.handle.lock().await.as_ref().cloned() {
            handle.terminate().await?;
        }
        Ok(())
    }

    pub(crate) async fn stderr_tail(&self) -> String {
        if let Some(stderr) = self.stderr.lock().await.as_ref().cloned() {
            return stderr.tail_string().await;
        }
        String::new()
    }
}

pub(crate) struct SandboxAcpTransport {
    command: AcpCommand,
    cwd:     String,
    env:     HashMap<String, String>,
    sandbox: Arc<dyn Sandbox>,
    state:   TransportState,
}

impl SandboxAcpTransport {
    pub(crate) fn new(
        command: AcpCommand,
        cwd: String,
        env: HashMap<String, String>,
        sandbox: Arc<dyn Sandbox>,
        state: TransportState,
    ) -> Self {
        Self {
            command,
            cwd,
            env,
            sandbox,
            state,
        }
    }
}

impl ConnectTo<Client> for SandboxAcpTransport {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> AcpProtocolResult<()> {
        let mut env = self.command.env().clone();
        env.extend(self.env);

        let process = match self
            .sandbox
            .spawn_stdio_process(
                &self.command.to_shell_command(),
                Some(&self.cwd),
                Some(&env),
                None,
            )
            .await
        {
            Ok(process) => process,
            Err(error) => {
                self.state.set_startup_error(error).await;
                return Err(internal_error("ACP process failed to start"));
            }
        };

        let handle = process.handle.clone();
        let stderr = process.stderr.clone();
        self.state.set_process(handle.clone(), stderr.clone()).await;

        let incoming_lines = Box::pin(BufReader::new(process.stdout.compat()).lines())
            as Pin<Box<dyn Stream<Item = IoResult<String>> + Send>>;
        let outgoing_sink = Box::pin(unfold(
            process.stdin.compat_write(),
            async move |mut writer, line: String| {
                let mut bytes = line.into_bytes();
                bytes.push(b'\n');
                writer.write_all(&bytes).await?;
                Ok::<_, std::io::Error>(writer)
            },
        ));

        let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
            Lines::new(outgoing_sink, incoming_lines),
            client,
        );
        tokio::select! {
            result = protocol => {
                if let Err(err) = handle.terminate().await {
                    tracing::warn!(error = %err, "Failed to terminate ACP process after protocol completion");
                }
                let _ = timeout(Duration::from_millis(500), handle.wait()).await;
                result
            }
            termination = handle.wait() => {
                let termination = termination.map_err(ProtocolError::into_internal_error)?;
                let stderr = stderr.tail_string().await;
                let exit_code = termination
                    .exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string());
                Err(internal_error(format!(
                    "ACP process exited before protocol completed: termination={}, exit_code={exit_code}, stderr={stderr}",
                    termination.termination,
                )))
            }
        }
    }
}
