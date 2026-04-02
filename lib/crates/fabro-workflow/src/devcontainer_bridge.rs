use std::time::Instant;

use sha2::{Digest, Sha256};

use fabro_devcontainer::DevcontainerSpec;

use crate::event::{EventEmitter, WorkflowRunEvent};
use fabro_agent::sandbox::Sandbox;
use fabro_sandbox::daytona::{DaytonaSnapshotConfig, DockerfileSource};
use futures::future::try_join_all;

/// Compute a deterministic snapshot name from Dockerfile content.
pub fn snapshot_name_for_dockerfile(dockerfile: &str) -> String {
    let hash = Sha256::digest(dockerfile.as_bytes());
    let hex = hex::encode(hash);
    format!("devcontainer-{}", &hex[..12])
}

/// Map a `DevcontainerSpec` to a `DaytonaSnapshotConfig`.
pub fn devcontainer_to_snapshot_config(dc: &DevcontainerSpec) -> DaytonaSnapshotConfig {
    DaytonaSnapshotConfig {
        name: snapshot_name_for_dockerfile(&dc.dockerfile),
        dockerfile: Some(DockerfileSource::Inline(dc.dockerfile.clone())),
        cpu: None,
        memory: None,
        disk: None,
    }
}

/// Run a set of devcontainer lifecycle commands inside a sandbox.
///
/// Follows the same pattern as setup commands in `run.rs`.
pub async fn run_devcontainer_lifecycle(
    sandbox: &dyn Sandbox,
    emitter: &EventEmitter,
    phase: &str,
    commands: &[fabro_devcontainer::Command],
    timeout_ms: u64,
) -> anyhow::Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    emitter.emit(&WorkflowRunEvent::DevcontainerLifecycleStarted {
        phase: phase.to_string(),
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
                )
                .await?;
            }
            fabro_devcontainer::Command::Args(args) => {
                let joined = args
                    .iter()
                    .map(|a| shlex::try_quote(a).unwrap_or_else(|_| a.into()).to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                run_single_lifecycle_command(sandbox, emitter, phase, &joined, index, timeout_ms)
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
                        async move {
                            let cmd_start = Instant::now();
                            emitter.emit(&WorkflowRunEvent::DevcontainerLifecycleCommandStarted {
                                phase: phase.clone(),
                                command: name.clone(),
                                index,
                            });
                            let result = sandbox
                                .exec_command(&command, timeout_ms, None, None, None)
                                .await
                                .map_err(|e| {
                                    anyhow::anyhow!(
                                        "Devcontainer {phase} parallel command '{name}' failed: {e}"
                                    )
                                })?;
                            let cmd_duration = crate::millis_u64(cmd_start.elapsed());
                            if result.exit_code != 0 {
                                emitter.emit(
                                    &WorkflowRunEvent::DevcontainerLifecycleFailed {
                                        phase: phase.clone(),
                                        command: name.clone(),
                                        index,
                                        exit_code: result.exit_code,
                                        stderr: result.stderr.clone(),
                                    },
                                );
                                anyhow::bail!(
                                    "Devcontainer {phase} parallel command '{name}' failed (exit code {}): {}",
                                    result.exit_code,
                                    result.stderr,
                                );
                            }
                            emitter.emit(
                                &WorkflowRunEvent::DevcontainerLifecycleCommandCompleted {
                                    phase: phase.clone(),
                                    command: name.clone(),
                                    index,
                                    exit_code: result.exit_code,
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
    emitter.emit(&WorkflowRunEvent::DevcontainerLifecycleCompleted {
        phase: phase.to_string(),
        duration_ms: phase_duration,
    });
    Ok(())
}

async fn run_single_lifecycle_command(
    sandbox: &dyn Sandbox,
    emitter: &EventEmitter,
    phase: &str,
    command: &str,
    index: usize,
    timeout_ms: u64,
) -> anyhow::Result<()> {
    emitter.emit(&WorkflowRunEvent::DevcontainerLifecycleCommandStarted {
        phase: phase.to_string(),
        command: command.to_string(),
        index,
    });
    let cmd_start = Instant::now();
    let result = sandbox
        .exec_command(command, timeout_ms, None, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("Devcontainer {phase} command failed: {e}"))?;
    let cmd_duration = crate::millis_u64(cmd_start.elapsed());
    if result.exit_code != 0 {
        emitter.emit(&WorkflowRunEvent::DevcontainerLifecycleFailed {
            phase: phase.to_string(),
            command: command.to_string(),
            index,
            exit_code: result.exit_code,
            stderr: result.stderr.clone(),
        });
        anyhow::bail!(
            "Devcontainer {phase} command failed (exit code {}): {command}\n{}",
            result.exit_code,
            result.stderr,
        );
    }
    emitter.emit(&WorkflowRunEvent::DevcontainerLifecycleCommandCompleted {
        phase: phase.to_string(),
        command: command.to_string(),
        index,
        exit_code: result.exit_code,
        duration_ms: cmd_duration,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use fabro_agent::sandbox::{ExecResult, GrepOptions, Sandbox};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    /// Simple test sandbox that records commands and returns a fixed exit code.
    struct TestSandbox {
        commands: Mutex<Vec<String>>,
        exit_code: i32,
    }

    impl TestSandbox {
        fn new() -> Self {
            Self {
                commands: Mutex::new(Vec::new()),
                exit_code: 0,
            }
        }

        fn with_exit_code(exit_code: i32) -> Self {
            Self {
                commands: Mutex::new(Vec::new()),
                exit_code,
            }
        }

        fn captured_commands(&self) -> Vec<String> {
            self.commands.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Sandbox for TestSandbox {
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> Result<String, String> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), String> {
            Ok(())
        }
        async fn delete_file(&self, _path: &str) -> Result<(), String> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, String> {
            Ok(false)
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> Result<Vec<fabro_agent::sandbox::DirEntry>, String> {
            Ok(vec![])
        }
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<CancellationToken>,
        ) -> Result<ExecResult, String> {
            self.commands.lock().unwrap().push(command.to_string());
            Ok(ExecResult {
                stdout: String::new(),
                stderr: if self.exit_code != 0 {
                    "command failed".to_string()
                } else {
                    String::new()
                },
                exit_code: self.exit_code,
                timed_out: false,
                duration_ms: 10,
            })
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &GrepOptions,
        ) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn glob(&self, _pattern: &str, _path: Option<&str>) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn download_file_to_local(
            &self,
            _remote_path: &str,
            _local_path: &std::path::Path,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn upload_file_from_local(
            &self,
            _local_path: &std::path::Path,
            _remote_path: &str,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn initialize(&self) -> Result<(), String> {
            Ok(())
        }
        async fn cleanup(&self) -> Result<(), String> {
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
        let emitter = EventEmitter::default();
        let commands = vec![fabro_devcontainer::Command::Shell("echo hi".to_string())];
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000)
            .await
            .unwrap();
        let captured = sandbox.captured_commands();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].contains("echo hi"), "command: {}", captured[0]);
    }

    #[tokio::test]
    async fn args_command_joins() {
        let sandbox = TestSandbox::new();
        let emitter = EventEmitter::default();
        let commands = vec![fabro_devcontainer::Command::Args(vec![
            "echo".to_string(),
            "hi".to_string(),
        ])];
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000)
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
        let emitter = EventEmitter::default();
        let events = Arc::new(Mutex::new(Vec::<crate::event::RunEventEnvelope>::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
        let sandbox = TestSandbox::new();
        let commands = vec![fabro_devcontainer::Command::Shell("echo hi".to_string())];
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000)
            .await
            .unwrap();
        let events = events.lock().unwrap();
        assert_eq!(events[0].event, "devcontainer.lifecycle.started");
        assert_eq!(events[0].properties["phase"], "on_create");
        assert_eq!(events[0].properties["command_count"], 1);

        assert_eq!(events[1].event, "devcontainer.lifecycle.command.started");
        assert_eq!(events[1].properties["phase"], "on_create");
        assert_eq!(events[1].properties["index"], 0);

        assert_eq!(events[2].event, "devcontainer.lifecycle.command.completed");
        assert_eq!(events[2].properties["phase"], "on_create");
        assert_eq!(events[2].properties["index"], 0);
        assert_eq!(events[2].properties["exit_code"], 0);

        assert_eq!(events[3].event, "devcontainer.lifecycle.completed");
        assert_eq!(events[3].properties["phase"], "on_create");
    }

    #[tokio::test]
    async fn failed_command_emits_failed_and_returns_error() {
        let emitter = EventEmitter::default();
        let events = Arc::new(Mutex::new(Vec::<crate::event::RunEventEnvelope>::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
        let sandbox = TestSandbox::with_exit_code(1);
        let commands = vec![fabro_devcontainer::Command::Shell("false".to_string())];
        let result =
            run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &commands, 300_000).await;
        assert!(result.is_err());
        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| {
            event.event == "devcontainer.lifecycle.failed"
                && event.properties["phase"] == "on_create"
                && event.properties["exit_code"] == 1
        }));
    }

    #[tokio::test]
    async fn empty_commands_is_noop() {
        let emitter = EventEmitter::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        });
        let sandbox = TestSandbox::new();
        run_devcontainer_lifecycle(&sandbox, &emitter, "on_create", &[], 300_000)
            .await
            .unwrap();
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn parallel_commands_run() {
        let sandbox = TestSandbox::new();
        let emitter = EventEmitter::default();
        let mut map = HashMap::new();
        map.insert("install".to_string(), "npm install".to_string());
        map.insert("build".to_string(), "npm run build".to_string());
        let commands = vec![fabro_devcontainer::Command::Parallel(map)];
        run_devcontainer_lifecycle(&sandbox, &emitter, "post_create", &commands, 300_000)
            .await
            .unwrap();
        let captured = sandbox.captured_commands();
        assert_eq!(captured.len(), 2);
    }

    fn test_devcontainer_config(dockerfile: &str) -> DevcontainerSpec {
        DevcontainerSpec {
            dockerfile: dockerfile.to_string(),
            build_context: std::path::PathBuf::from("."),
            build_args: HashMap::new(),
            build_target: None,
            initialize_commands: vec![],
            on_create_commands: vec![],
            post_create_commands: vec![],
            post_start_commands: vec![],
            environment: HashMap::new(),
            container_env: HashMap::new(),
            remote_user: None,
            workspace_folder: "/workspaces/test".to_string(),
            forwarded_ports: vec![],
            compose_files: vec![],
            compose_service: None,
        }
    }
}
