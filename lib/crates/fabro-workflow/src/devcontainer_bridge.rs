use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use fabro_agent::sandbox::Sandbox;
use fabro_devcontainer::DevcontainerSpec;
use fabro_sandbox::daytona::{DaytonaSnapshotConfig, DockerfileSource};
use futures::future::try_join_all;
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::event::{Emitter, Event};
use crate::handler::sandbox_cancel_token;

/// Compute a deterministic snapshot name from Dockerfile content.
pub fn snapshot_name_for_dockerfile(dockerfile: &str) -> String {
    let hash = Sha256::digest(dockerfile.as_bytes());
    let hex = hex::encode(hash);
    format!("devcontainer-{}", &hex[..12])
}

/// Map a `DevcontainerSpec` to a `DaytonaSnapshotConfig`.
pub fn devcontainer_to_snapshot_config(dc: &DevcontainerSpec) -> DaytonaSnapshotConfig {
    DaytonaSnapshotConfig {
        name:       snapshot_name_for_dockerfile(&dc.dockerfile),
        dockerfile: Some(DockerfileSource::Inline(dc.dockerfile.clone())),
        cpu:        None,
        memory:     None,
        disk:       None,
    }
}

/// Run a set of devcontainer lifecycle commands inside a sandbox.
///
/// Follows the same pattern as setup commands in `run.rs`.
pub async fn run_devcontainer_lifecycle(
    sandbox: &dyn Sandbox,
    emitter: &Emitter,
    phase: &str,
    commands: &[fabro_devcontainer::Command],
    timeout_ms: u64,
    cancel_requested: Option<Arc<AtomicBool>>,
) -> Result<(), Error> {
    if commands.is_empty() {
        return Ok(());
    }

    emitter.emit(&Event::DevcontainerLifecycleStarted {
        phase:         phase.to_string(),
        command_count: commands.len(),
    });
    let phase_start = Instant::now();

    for (index, cmd) in commands.iter().enumerate() {
        match cmd {
            fabro_devcontainer::Command::Shell(s) => {
                run_single_lifecycle_command(
                    sandbox,
                    emitter,
                    phase,
                    &format!("sh -c {}", shlex::try_quote(s).unwrap_or_else(|_| s.into())),
                    index,
                    timeout_ms,
                    cancel_requested.clone(),
                )
                .await?;
            }
            fabro_devcontainer::Command::Args(args) => {
                let joined = args
                    .iter()
                    .map(|a| shlex::try_quote(a).unwrap_or_else(|_| a.into()).to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                run_single_lifecycle_command(
                    sandbox,
                    emitter,
                    phase,
                    &joined,
                    index,
                    timeout_ms,
                    cancel_requested.clone(),
                )
                .await?;
            }
            fabro_devcontainer::Command::Parallel(map) => {
                let futs: Vec<_> = map
                    .iter()
                    .map(|(name, cmd_str)| {
                        let command = format!(
                            "sh -c {}",
                            shlex::try_quote(cmd_str).unwrap_or_else(|_| cmd_str.into())
                        );
                        let phase = phase.to_string();
                        let name = name.clone();
                        let cancel_requested = cancel_requested.clone();
                        async move {
                            let cmd_start = Instant::now();
                            emitter.emit(&Event::DevcontainerLifecycleCommandStarted {
                                phase: phase.clone(),
                                command: name.clone(),
                                index,
                            });
                            let cancel_token = sandbox_cancel_token(cancel_requested);
                            let result = sandbox
                                .exec_command(
                                    &command,
                                    timeout_ms,
                                    None,
                                    None,
                                    cancel_token.clone(),
                                )
                                .await
                                .map_err(|e| {
                                    Error::engine(format!(
                                        "Devcontainer {phase} parallel command '{name}' failed: {e}"
                                    ))
                                })?;
                            if let Some(token) = &cancel_token {
                                if token.is_cancelled() {
                                    return Err(Error::Cancelled);
                                }
                                token.cancel();
                            }
                            let cmd_duration = crate::millis_u64(cmd_start.elapsed());
                            if !result.is_success() {
                                let exit_code = result.display_exit_code();
                                let exec_output_tail = result.default_redacted_output_tail();
                                emitter.emit(
                                    &Event::DevcontainerLifecycleFailed {
                                        phase: phase.clone(),
                                        command: name.clone(),
                                        index,
                                        exit_code,
                                        stderr: result.stderr.clone(),
                                        exec_output_tail,
                                    },
                                );
                                return Err(Error::engine(format!(
                                    "Devcontainer {phase} parallel command '{name}' failed (exit code {}): {}",
                                    exit_code,
                                    result.stderr,
                                )));
                            }
                            let exit_code = result.exit_code.unwrap_or(0);
                            emitter.emit(
                                &Event::DevcontainerLifecycleCommandCompleted {
                                    phase: phase.clone(),
                                    command: name.clone(),
                                    index,
                                    exit_code,
                                    duration_ms: cmd_duration,
                                },
                            );
                            Ok(())
                        }
                    })
                    .collect();
                try_join_all(futs).await?;
            }
        }
    }

    let phase_duration = crate::millis_u64(phase_start.elapsed());
    emitter.emit(&Event::DevcontainerLifecycleCompleted {
        phase:       phase.to_string(),
        duration_ms: phase_duration,
    });
    Ok(())
}

async fn run_single_lifecycle_command(
    sandbox: &dyn Sandbox,
    emitter: &Emitter,
    phase: &str,
    command: &str,
    index: usize,
    timeout_ms: u64,
    cancel_requested: Option<Arc<AtomicBool>>,
) -> Result<(), Error> {
    emitter.emit(&Event::DevcontainerLifecycleCommandStarted {
        phase: phase.to_string(),
        command: command.to_string(),
        index,
    });
    let cmd_start = Instant::now();
    let cancel_token = sandbox_cancel_token(cancel_requested);
    let result = sandbox
        .exec_command(command, timeout_ms, None, None, cancel_token.clone())
        .await
        .map_err(|e| Error::engine(format!("Devcontainer {phase} command failed: {e}")))?;
    if let Some(token) = &cancel_token {
        if token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        token.cancel();
    }
    let cmd_duration = crate::millis_u64(cmd_start.elapsed());
    if !result.is_success() {
        let exit_code = result.display_exit_code();
        let exec_output_tail = result.default_redacted_output_tail();
        emitter.emit(&Event::DevcontainerLifecycleFailed {
            phase: phase.to_string(),
            command: command.to_string(),
            index,
            exit_code,
            stderr: result.stderr.clone(),
            exec_output_tail,
        });
        return Err(Error::engine(format!(
            "Devcontainer {phase} command failed (exit code {}): {command}\n{}",
            exit_code, result.stderr,
        )));
    }
    let exit_code = result.exit_code.unwrap_or(0);
    emitter.emit(&Event::DevcontainerLifecycleCommandCompleted {
        phase: phase.to_string(),
        command: command.to_string(),
        index,
        exit_code,
        duration_ms: cmd_duration,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use fabro_agent::sandbox::{ExecResult, GrepOptions, Sandbox};
    use fabro_types::{CommandTermination, EventBody};
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Simple test sandbox that records commands and returns a fixed exit code.
    struct TestSandbox {
        commands:        Mutex<Vec<String>>,
        cancel_tokens:   Mutex<Vec<bool>>,
        exit_code:       i32,
        wait_for_cancel: bool,
    }

    impl TestSandbox {
        fn new() -> Self {
            Self {
                commands:        Mutex::new(Vec::new()),
                cancel_tokens:   Mutex::new(Vec::new()),
                exit_code:       0,
                wait_for_cancel: false,
            }
        }

        fn with_exit_code(exit_code: i32) -> Self {
            Self {
                commands: Mutex::new(Vec::new()),
                cancel_tokens: Mutex::new(Vec::new()),
                exit_code,
                wait_for_cancel: false,
            }
        }

        fn waiting_for_cancel() -> Self {
            Self {
                commands:        Mutex::new(Vec::new()),
                cancel_tokens:   Mutex::new(Vec::new()),
                exit_code:       0,
                wait_for_cancel: true,
            }
        }

        fn captured_commands(&self) -> Vec<String> {
            self.commands.lock().unwrap().clone()
        }

        fn captured_cancel_tokens(&self) -> Vec<bool> {
            self.cancel_tokens.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Sandbox for TestSandbox {
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> fabro_sandbox::Result<String> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn delete_file(&self, _path: &str) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> fabro_sandbox::Result<bool> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> fabro_sandbox::Result<Vec<fabro_agent::sandbox::DirEntry>> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            cancel_token: Option<CancellationToken>,
        ) -> fabro_sandbox::Result<ExecResult> {
            self.commands.lock().unwrap().push(command.to_string());
            self.cancel_tokens
                .lock()
                .unwrap()
                .push(cancel_token.is_some());
            if self.wait_for_cancel {
                let token = cancel_token
                    .ok_or_else(|| fabro_sandbox::Error::message("missing cancel token"))?;
                token.cancelled().await;
                return Ok(ExecResult {
                    stdout:      String::new(),
                    stderr:      "cancelled".to_string(),
                    exit_code:   None,
                    termination: CommandTermination::Cancelled,
                    duration_ms: 10,
                });
            }
            Ok(ExecResult {
                stdout:      String::new(),
                stderr:      if self.exit_code != 0 {
                    "command failed".to_string()
                } else {
                    String::new()
                },
                exit_code:   Some(self.exit_code),
                termination: CommandTermination::Exited,
                duration_ms: 10,
            })
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &GrepOptions,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> fabro_sandbox::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn download_file_to_local(
            &self,
            _remote_path: &str,
            _local_path: &std::path::Path,
        ) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn upload_file_from_local(
            &self,
            _local_path: &std::path::Path,
            _remote_path: &str,
        ) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn initialize(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        async fn cleanup(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }
        fn working_directory(&self) -> &str {
            "/work"
        }
        fn platform(&self) -> &str {
            "linux"
        }
        fn os_version(&self) -> String {
            "Linux 6.1.0".into()
        }
    }

    #[test]
    fn snapshot_name_is_deterministic() {
        let dockerfile = "FROM ubuntu:22.04\nRUN apt-get update";
        let name1 = snapshot_name_for_dockerfile(dockerfile);
        let name2 = snapshot_name_for_dockerfile(dockerfile);
        assert_eq!(name1, name2);
    }

    #[test]
    fn snapshot_name_differs_for_different_dockerfiles() {
        let name1 = snapshot_name_for_dockerfile("FROM ubuntu:22.04");
        let name2 = snapshot_name_for_dockerfile("FROM rust:1.85");
        assert_ne!(name1, name2);
    }

    #[test]
    fn snapshot_name_has_prefix() {
        let name = snapshot_name_for_dockerfile("FROM ubuntu:22.04");
        assert!(name.starts_with("devcontainer-"), "name: {name}");
        // prefix + 12 hex chars
        assert_eq!(name.len(), "devcontainer-".len() + 12);
    }

    #[test]
    fn maps_dockerfile_to_inline() {
        let dc = test_devcontainer_config("FROM rust:1.85\nRUN cargo install sccache");
        let snapshot = devcontainer_to_snapshot_config(&dc);
        assert_eq!(
            snapshot.dockerfile,
            Some(DockerfileSource::Inline(dc.dockerfile.clone()))
        );
    }

    #[test]
    fn snapshot_name_from_dockerfile_hash() {
        let dc = test_devcontainer_config("FROM ubuntu:22.04");
        let snapshot = devcontainer_to_snapshot_config(&dc);
        let expected = snapshot_name_for_dockerfile(&dc.dockerfile);
        assert_eq!(snapshot.name, expected);
    }

    #[tokio::test]
    async fn shell_command_executed() {
        let sandbox = TestSandbox::new();
        let emitter = Emitter::default();
        let commands = vec![fabro_devcontainer::Command::Shell("echo hi".to_string())];
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000, None)
            .await
            .unwrap();
        let captured = sandbox.captured_commands();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].contains("echo hi"), "command: {}", captured[0]);
    }

    #[tokio::test]
    async fn args_command_joins() {
        let sandbox = TestSandbox::new();
        let emitter = Emitter::default();
        let commands = vec![fabro_devcontainer::Command::Args(vec![
            "echo".to_string(),
            "hi".to_string(),
        ])];
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000, None)
            .await
            .unwrap();
        let captured = sandbox.captured_commands();
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0].contains("echo") && captured[0].contains("hi"),
            "command: {}",
            captured[0]
        );
    }

    #[tokio::test]
    async fn emits_started_and_completed_events() {
        let emitter = Emitter::default();
        let events = Arc::new(Mutex::new(Vec::<fabro_types::RunEvent>::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
        let sandbox = TestSandbox::new();
        let commands = vec![fabro_devcontainer::Command::Shell("echo hi".to_string())];
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000, None)
            .await
            .unwrap();
        let events = events.lock().unwrap();
        let started = events[0].properties().unwrap();
        assert_eq!(events[0].event_name(), "devcontainer.lifecycle.started");
        assert_eq!(started["phase"], "on_create");
        assert_eq!(started["command_count"], 1);

        assert_eq!(
            events[1].event_name(),
            "devcontainer.lifecycle.command.started"
        );
        let command_started = events[1].properties().unwrap();
        assert_eq!(command_started["phase"], "on_create");
        assert_eq!(command_started["index"], 0);

        assert_eq!(
            events[2].event_name(),
            "devcontainer.lifecycle.command.completed"
        );
        let command_completed = events[2].properties().unwrap();
        assert_eq!(command_completed["phase"], "on_create");
        assert_eq!(command_completed["index"], 0);
        assert_eq!(command_completed["exit_code"], 0);

        assert_eq!(events[3].event_name(), "devcontainer.lifecycle.completed");
        assert_eq!(events[3].properties().unwrap()["phase"], "on_create");
    }

    #[tokio::test]
    async fn failed_command_emits_failed_and_returns_error() {
        let emitter = Emitter::default();
        let events = Arc::new(Mutex::new(Vec::<fabro_types::RunEvent>::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
        let sandbox = TestSandbox::with_exit_code(1);
        let commands = vec![fabro_devcontainer::Command::Shell("false".to_string())];
        let result =
            run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000, None)
                .await;
        assert!(result.is_err());
        let events = events.lock().unwrap();
        let failed = events
            .iter()
            .find(|event| event.event_name() == "devcontainer.lifecycle.failed")
            .expect("devcontainer lifecycle failed event");
        match &failed.body {
            EventBody::DevcontainerLifecycleFailed(props) => {
                assert_eq!(props.phase, "on_create");
                assert_eq!(props.exit_code, 1);
                assert_eq!(props.stderr, "command failed");
                assert_eq!(
                    props
                        .exec_output_tail
                        .as_ref()
                        .and_then(|tail| tail.stderr.as_deref()),
                    Some("command failed")
                );
            }
            other => panic!("expected devcontainer lifecycle failed body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_commands_is_noop() {
        let emitter = Emitter::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
        let sandbox = TestSandbox::new();
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &[], 300_000, None)
            .await
            .unwrap();
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn parallel_commands_run() {
        let sandbox = TestSandbox::new();
        let emitter = Emitter::default();
        let mut map = HashMap::new();
        map.insert("install".to_string(), "npm install".to_string());
        map.insert("build".to_string(), "npm run build".to_string());
        let commands = vec![fabro_devcontainer::Command::Parallel(map)];
        run_devcontainer_lifecycle(&sandbox, &emitter, "post_create", &commands, 300_000, None)
            .await
            .unwrap();
        let captured = sandbox.captured_commands();
        assert_eq!(captured.len(), 2);
    }

    #[tokio::test]
    async fn cancelled_shell_command_returns_cancelled() {
        let sandbox = TestSandbox::waiting_for_cancel();
        let emitter = Emitter::default();
        let commands = vec![fabro_devcontainer::Command::Shell("sleep 5".to_string())];
        let cancel_requested = Arc::new(AtomicBool::new(true));

        let result = run_devcontainer_lifecycle(
            &sandbox,
            &emitter,
            "on_create",
            &commands,
            300_000,
            Some(cancel_requested),
        )
        .await;

        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(sandbox.captured_cancel_tokens(), vec![true]);
    }

    #[tokio::test]
    async fn cancelled_parallel_command_returns_cancelled() {
        let sandbox = TestSandbox::waiting_for_cancel();
        let emitter = Emitter::default();
        let mut map = HashMap::new();
        map.insert("install".to_string(), "sleep 5".to_string());
        map.insert("build".to_string(), "sleep 5".to_string());
        let commands = vec![fabro_devcontainer::Command::Parallel(map)];
        let cancel_requested = Arc::new(AtomicBool::new(true));

        let result = run_devcontainer_lifecycle(
            &sandbox,
            &emitter,
            "post_create",
            &commands,
            300_000,
            Some(cancel_requested),
        )
        .await;

        assert!(matches!(result, Err(Error::Cancelled)));
        let captured = sandbox.captured_cancel_tokens();
        assert!(!captured.is_empty());
        assert!(captured.iter().all(|saw_token| *saw_token));
    }

    fn test_devcontainer_config(dockerfile: &str) -> DevcontainerSpec {
        DevcontainerSpec {
            dockerfile:           dockerfile.to_string(),
            build_context:        std::path::PathBuf::from("."),
            build_args:           HashMap::new(),
            build_target:         None,
            initialize_commands:  vec![],
            on_create_commands:   vec![],
            post_create_commands: vec![],
            post_start_commands:  vec![],
            environment:          HashMap::new(),
            container_env:        HashMap::new(),
            remote_user:          None,
            workspace_folder:     "/workspaces/test".to_string(),
            forwarded_ports:      vec![],
            compose_files:        vec![],
            compose_service:      None,
        }
    }
}
