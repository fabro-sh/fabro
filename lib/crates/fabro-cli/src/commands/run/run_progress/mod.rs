#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI run-progress renderer: writes to std::io::stderr directly"
)]

use fabro_types::RunEvent;
use fabro_types::run_event::is_metadata_snapshot_compat_notice_code;

mod event;
mod info_display;
mod renderer;
mod setup_display;
mod stage_display;
mod styles;

use event::{ProgressEvent, from_json_line, from_run_event};
use info_display::InfoDisplay;
use renderer::ProgressRenderer;
use setup_display::SetupDisplay;
use stage_display::StageDisplay;

pub(crate) struct ProgressUI {
    renderer: ProgressRenderer,
    stage: StageDisplay,
    setup: SetupDisplay,
    info: InfoDisplay,
    saw_metadata_snapshot_failure: bool,
}

impl ProgressUI {
    pub(crate) fn new(is_tty: bool, verbose: bool) -> Self {
        let renderer = if is_tty {
            ProgressRenderer::new_tty()
        } else {
            ProgressRenderer::new_plain(
                Box::new(std::io::stderr()),
                console::colors_enabled_stderr(),
            )
        };
        Self::with_renderer(renderer, verbose)
    }

    fn with_renderer(renderer: ProgressRenderer, verbose: bool) -> Self {
        Self {
            renderer,
            stage: StageDisplay::new(verbose),
            setup: SetupDisplay::new(verbose),
            info: InfoDisplay::new(verbose),
            saw_metadata_snapshot_failure: false,
        }
    }

    #[cfg(test)]
    #[expect(
        clippy::disallowed_types,
        reason = "test helper accepts a sync blocking writer to capture rendered output"
    )]
    fn new_plain_test(out: Box<dyn std::io::Write + Send>, verbose: bool, colors: bool) -> Self {
        Self::with_renderer(ProgressRenderer::new_plain(out, colors), verbose)
    }

    pub(crate) fn set_working_directory(&mut self, dir: String) {
        self.stage.set_working_directory(dir);
    }

    pub(crate) fn hide_bars(&self) {
        self.renderer.hide();
    }

    pub(crate) fn show_bars(&self) {
        self.renderer.show();
    }

    pub(crate) fn finish(&mut self) {
        self.stage.finish();
        self.setup.finish();
        self.renderer.finish();
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Production code drives this via JSON lines; tests call it directly."
        )
    )]
    pub(crate) fn handle_event(&mut self, event: &RunEvent) {
        if let Some(progress_event) = from_run_event(event) {
            self.dispatch(progress_event);
        }
    }

    pub(crate) fn handle_json_line(&mut self, line: &str) {
        if let Some(progress_event) = from_json_line(line) {
            self.dispatch(progress_event);
        }
    }

    fn dispatch(&mut self, event: ProgressEvent) {
        let renderer = &self.renderer;
        match event {
            ProgressEvent::WorkflowStarted {
                worktree_dir,
                base_branch,
                base_sha,
            } => {
                if let Some(worktree_dir) = worktree_dir {
                    InfoDisplay::show_worktree(renderer, std::path::Path::new(&worktree_dir));
                }
                if let Some(base_sha) = base_sha {
                    InfoDisplay::show_base_info(renderer, base_branch.as_deref(), &base_sha);
                }
            }
            ProgressEvent::WorkingDirectorySet { working_directory } => {
                self.set_working_directory(working_directory);
            }
            ProgressEvent::SandboxInitializing { provider } => {
                self.setup.on_sandbox_initializing(renderer, &provider);
            }
            ProgressEvent::SandboxReady {
                provider,
                duration_ms,
                name,
                cpu,
                memory,
                url,
            } => {
                self.setup.on_sandbox_ready(
                    renderer,
                    &provider,
                    duration_ms,
                    name.as_deref(),
                    cpu,
                    memory,
                    url.as_deref(),
                );
            }
            ProgressEvent::SshAccessReady { ssh_command } => {
                SetupDisplay::on_ssh_access_ready(renderer, &ssh_command);
            }
            ProgressEvent::SetupStarted { command_count } => {
                self.setup.on_setup_started(renderer, command_count);
            }
            ProgressEvent::SetupCompleted { duration_ms } => {
                self.setup.on_setup_completed(renderer, duration_ms);
            }
            ProgressEvent::SetupCommandCompleted {
                command,
                command_index,
                exit_code,
                duration_ms,
            } => {
                self.setup.on_setup_command_completed(
                    renderer,
                    &command,
                    command_index,
                    exit_code,
                    duration_ms,
                );
            }
            ProgressEvent::CliEnsureStarted { cli_name } => {
                self.setup.on_cli_ensure_started(renderer, &cli_name);
            }
            ProgressEvent::CliEnsureCompleted {
                cli_name,
                already_installed,
                duration_ms,
            } => {
                self.setup.on_cli_ensure_completed(
                    renderer,
                    &cli_name,
                    already_installed,
                    duration_ms,
                );
            }
            ProgressEvent::CliEnsureFailed { cli_name } => {
                self.setup.on_cli_ensure_failed(renderer, &cli_name);
            }
            ProgressEvent::DevcontainerResolved {
                dockerfile_lines,
                environment_count,
                lifecycle_command_count,
                workspace_folder,
            } => {
                SetupDisplay::on_devcontainer_resolved(
                    renderer,
                    dockerfile_lines,
                    environment_count,
                    lifecycle_command_count,
                    &workspace_folder,
                );
            }
            ProgressEvent::DevcontainerLifecycleStarted {
                phase,
                command_count,
            } => {
                self.setup
                    .on_devcontainer_lifecycle_started(renderer, &phase, command_count);
            }
            ProgressEvent::DevcontainerLifecycleCompleted { phase, duration_ms } => {
                self.setup
                    .on_devcontainer_lifecycle_completed(renderer, &phase, duration_ms);
            }
            ProgressEvent::DevcontainerLifecycleFailed {
                phase,
                command,
                exit_code,
                stderr,
            } => {
                self.setup.on_devcontainer_lifecycle_failed(
                    renderer, &phase, &command, exit_code, &stderr,
                );
            }
            ProgressEvent::DevcontainerLifecycleCommandCompleted {
                command,
                command_index,
                exit_code,
                duration_ms,
            } => {
                self.setup.on_devcontainer_lifecycle_command_completed(
                    renderer,
                    &command,
                    command_index,
                    exit_code,
                    duration_ms,
                );
            }
            ProgressEvent::StageStarted {
                node_id,
                name,
                script,
            } => {
                self.stage
                    .on_stage_started(renderer, &node_id, &name, script.as_deref());
            }
            ProgressEvent::StageCompleted {
                node_id,
                name,
                duration_ms,
                status,
                usage,
            } => {
                self.stage.on_stage_completed(
                    renderer,
                    &node_id,
                    &name,
                    duration_ms,
                    &status,
                    usage.as_ref(),
                );
            }
            ProgressEvent::StageFailed {
                node_id,
                name,
                error,
            } => {
                self.stage
                    .on_stage_failed(renderer, &node_id, &name, &error);
            }
            ProgressEvent::StageRetrying {
                name,
                attempt,
                max_attempts,
                delay_ms,
            } => {
                self.info
                    .on_stage_retrying(renderer, &name, attempt, max_attempts, delay_ms);
            }
            ProgressEvent::ParallelStarted => {
                self.stage.on_parallel_started();
            }
            ProgressEvent::ParallelBranchStarted { branch } => {
                self.stage.on_parallel_branch_started(renderer, &branch);
            }
            ProgressEvent::ParallelBranchCompleted {
                branch,
                duration_ms,
                status,
            } => {
                self.stage
                    .on_parallel_branch_completed(renderer, &branch, duration_ms, &status);
            }
            ProgressEvent::ParallelCompleted => {
                self.stage.on_parallel_completed();
            }
            ProgressEvent::AssistantMessage {
                stage_node_id,
                model,
            } => {
                self.stage
                    .on_assistant_message(renderer, &stage_node_id, &model);
            }
            ProgressEvent::ToolCallStarted {
                stage_node_id,
                tool_name,
                tool_call_id,
                arguments,
                timestamp,
            } => {
                self.stage.on_tool_call_started(
                    renderer,
                    &stage_node_id,
                    &tool_name,
                    &tool_call_id,
                    &arguments,
                    timestamp,
                );
            }
            ProgressEvent::ToolCallCompleted {
                stage_node_id,
                tool_call_id,
                is_error,
                duration_ms,
                timestamp,
            } => {
                self.stage.on_tool_call_completed(
                    renderer,
                    &stage_node_id,
                    &tool_call_id,
                    is_error,
                    duration_ms,
                    timestamp,
                );
            }
            ProgressEvent::ContextWindowWarning {
                stage_node_id,
                usage_percent,
            } => {
                self.stage
                    .on_context_window_warning(renderer, &stage_node_id, usage_percent);
            }
            ProgressEvent::CompactionStarted { stage_node_id } => {
                self.stage.on_compaction_started(renderer, &stage_node_id);
            }
            ProgressEvent::CompactionCompleted {
                stage_node_id,
                original_turn_count,
                preserved_turn_count,
                tracked_file_count,
            } => {
                self.stage.on_compaction_completed(
                    renderer,
                    &stage_node_id,
                    original_turn_count,
                    preserved_turn_count,
                    tracked_file_count,
                );
            }
            ProgressEvent::LlmRetry {
                stage_node_id,
                model,
                attempt,
                delay_ms,
                error,
            } => {
                self.stage.on_llm_retry(
                    renderer,
                    &stage_node_id,
                    &model,
                    attempt,
                    delay_ms,
                    &error,
                );
            }
            ProgressEvent::SubagentSpawned {
                stage_node_id,
                agent_id,
                task,
            } => {
                self.stage
                    .on_subagent_spawned(renderer, &stage_node_id, &agent_id, &task);
            }
            ProgressEvent::SubagentCompleted {
                stage_node_id,
                agent_id,
                success,
                turns_used,
            } => {
                self.stage.on_subagent_completed(
                    renderer,
                    &stage_node_id,
                    &agent_id,
                    success,
                    turns_used,
                );
            }
            ProgressEvent::EdgeSelected {
                from_node,
                to_node,
                label,
                condition,
            } => {
                self.info.on_edge_selected(
                    renderer,
                    &from_node,
                    &to_node,
                    label.as_deref(),
                    condition.as_deref(),
                );
            }
            ProgressEvent::LoopRestart { from_node, to_node } => {
                self.info.on_loop_restart(renderer, &from_node, &to_node);
            }
            ProgressEvent::RetroStarted => {
                self.stage.on_retro_started(renderer);
            }
            ProgressEvent::RetroCompleted { duration_ms } => {
                self.stage.on_retro_completed(renderer, duration_ms);
            }
            ProgressEvent::RetroFailed { duration_ms } => {
                self.stage.on_retro_failed(renderer, duration_ms);
            }
            ProgressEvent::MetadataSnapshotCompleted { phase, duration_ms } => {
                InfoDisplay::on_metadata_snapshot_completed(renderer, &phase, duration_ms);
            }
            ProgressEvent::MetadataSnapshotFailed {
                phase,
                failure_kind,
                error,
            } => {
                self.saw_metadata_snapshot_failure = true;
                InfoDisplay::on_metadata_snapshot_failed(renderer, &phase, &failure_kind, &error);
            }
            ProgressEvent::RunNotice {
                level,
                code,
                message,
            } => {
                if self.saw_metadata_snapshot_failure
                    && is_metadata_snapshot_compat_notice_code(&code)
                {
                    return;
                }
                InfoDisplay::on_run_notice(renderer, level, &code, &message);
            }
            ProgressEvent::PullRequestCreated { pr_url, draft } => {
                InfoDisplay::on_pull_request_created(renderer, &pr_url, draft);
            }
            ProgressEvent::PullRequestFailed { error } => {
                InfoDisplay::on_pull_request_failed(renderer, &error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::absolute_paths,
        clippy::needless_pass_by_value,
        reason = "These run-progress tests prefer explicit fixtures over pedantic style lints."
    )]
    #![expect(
        clippy::disallowed_types,
        reason = "These tests capture rendered output in Vec<u8> buffers."
    )]

    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, Utc};
    use fabro_agent::{AgentEvent, SandboxEvent};
    use fabro_llm::types::TokenCounts;
    use fabro_model::Provider;
    use fabro_types::{
        MetadataSnapshotFailureKind, MetadataSnapshotPhase, ParallelBranchId, StageId, fixtures,
    };
    use fabro_workflow::event::{Event, RunNoticeLevel, to_run_event, to_run_event_at};
    use fabro_workflow::outcome::billed_model_usage_from_llm;

    use super::*;
    use crate::commands::run::run_progress::stage_display::ToolCallStatus;

    struct SharedBuffer {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner
                .lock()
                .expect("buffer lock poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture_ui(verbose: bool) -> (ProgressUI, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let ui = ProgressUI::new_plain_test(
            Box::new(SharedBuffer {
                inner: Arc::clone(&buffer),
            }),
            verbose,
            false,
        );
        (ui, buffer)
    }

    fn rendered(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().expect("buffer lock poisoned").clone())
            .expect("valid utf-8")
    }

    fn emit(ui: &mut ProgressUI, event: Event) {
        let stored = to_run_event(&fixtures::RUN_1, &event);
        ui.handle_event(&stored);
    }

    fn emit_ref(ui: &mut ProgressUI, event: &Event) {
        let stored = to_run_event(&fixtures::RUN_1, event);
        ui.handle_event(&stored);
    }

    fn agent_event(stage: &str, event: AgentEvent) -> Event {
        Event::Agent {
            stage: stage.into(),
            visit: 1,
            event,
            session_id: None,
            parent_session_id: None,
        }
    }

    fn stage_started(node_id: &str, name: &str) -> Event {
        Event::StageStarted {
            node_id:      node_id.into(),
            name:         name.into(),
            index:        0,
            handler_type: String::new(),
            attempt:      1,
            max_attempts: 1,
        }
    }

    fn assistant_message(stage: &str, model: &str) -> Event {
        agent_event(stage, AgentEvent::AssistantMessage {
            text:            "done".into(),
            model:           model.into(),
            usage:           TokenCounts::default(),
            tool_call_count: 0,
        })
    }

    fn stage_completed(node_id: &str, name: &str) -> Event {
        Event::StageCompleted {
            node_id: node_id.into(),
            name: name.into(),
            index: 0,
            duration_ms: 5000,
            status: "success".into(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: Some(billed_model_usage_from_llm(
                "gpt-5-mini",
                Provider::OpenAi,
                None,
                &TokenCounts {
                    input_tokens: 1200,
                    output_tokens: 300,
                    ..TokenCounts::default()
                },
            )),
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: None,
            restart_failure_signatures: None,
            response: None,
            attempt: 1,
            max_attempts: 1,
        }
    }

    #[test]
    fn parallel_branches_tracked_as_tool_calls() {
        let mut ui = ProgressUI::new(true, false);

        emit(&mut ui, stage_started("fork1", "Fork Analysis"));
        assert!(ui.stage.active_stages.contains_key("fork1"));
        assert!(ui.stage.parallel_parent.is_none());

        emit(&mut ui, Event::ParallelStarted {
            node_id:      "fork1".into(),
            visit:        1,
            branch_count: 2,
            join_policy:  "wait_all".into(),
        });
        assert_eq!(ui.stage.parallel_parent.as_deref(), Some("fork1"));

        emit(&mut ui, Event::ParallelBranchStarted {
            parallel_group_id:  StageId::new("fork1", 1),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fork1", 1), 0),
            branch:             "security".into(),
            index:              0,
        });
        let stage = &ui.stage.active_stages["fork1"];
        assert_eq!(stage.tool_calls.len(), 1);
        assert_eq!(stage.tool_calls[0].tool_call_id, "security");
        assert!(matches!(
            stage.tool_calls[0].status,
            ToolCallStatus::Running
        ));

        emit(&mut ui, Event::ParallelBranchCompleted {
            parallel_group_id:  StageId::new("fork1", 1),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fork1", 1), 0),
            branch:             "security".into(),
            index:              0,
            duration_ms:        2000,
            status:             "success".into(),
            head_sha:           None,
        });
        let stage = &ui.stage.active_stages["fork1"];
        assert!(matches!(
            stage.tool_calls[0].status,
            ToolCallStatus::Succeeded
        ));
    }

    #[test]
    fn parallel_branch_running_shows_triangle_glyph() {
        let mut ui = ProgressUI::new(true, false);

        emit(&mut ui, stage_started("fork1", "Fork"));
        emit(&mut ui, Event::ParallelStarted {
            node_id:      "fork1".into(),
            visit:        1,
            branch_count: 1,
            join_policy:  "wait_all".into(),
        });
        emit(&mut ui, Event::ParallelBranchStarted {
            parallel_group_id:  StageId::new("fork1", 1),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fork1", 1), 0),
            branch:             "security".into(),
            index:              0,
        });

        let stage = &ui.stage.active_stages["fork1"];
        let message = stage.tool_calls[0].bar.message();
        assert!(
            message.contains('\u{25b8}'),
            "expected branch message to contain ▸, got: {message:?}"
        );
    }

    #[test]
    fn compaction_sets_and_clears_bar() {
        let mut ui = ProgressUI::new(true, false);

        emit(&mut ui, stage_started("s1", "Build"));
        assert!(ui.stage.active_stages["s1"].compaction_bar.is_none());

        emit(
            &mut ui,
            agent_event("s1", AgentEvent::CompactionStarted {
                estimated_tokens:    5000,
                context_window_size: 8000,
            }),
        );
        assert!(ui.stage.active_stages["s1"].compaction_bar.is_some());

        emit(
            &mut ui,
            agent_event("s1", AgentEvent::CompactionCompleted {
                original_turn_count:    20,
                preserved_turn_count:   6,
                summary_token_estimate: 500,
                tracked_file_count:     3,
            }),
        );
        assert!(ui.stage.active_stages["s1"].compaction_bar.is_none());
    }

    #[test]
    fn handle_json_line_ignores_invalid_json() {
        let (mut ui, buffer) = capture_ui(false);
        ui.handle_json_line("not valid json");
        ui.handle_json_line("");
        ui.handle_json_line("{}");
        assert!(rendered(&buffer).is_empty());
    }

    #[test]
    fn handle_json_line_matches_handle_event_for_verbose_events() {
        let events = vec![
            stage_started("code", "Code"),
            Event::SandboxInitialized {
                working_directory: "/home/daytona/workspace".into(),
                provider:          "daytona".into(),
                identifier:        None,
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
            },
            agent_event("code", AgentEvent::ToolCallStarted {
                tool_name:    "read_file".into(),
                tool_call_id: "tc1".into(),
                arguments:    serde_json::json!({
                    "file_path": "/home/daytona/workspace/src/main.rs"
                }),
            }),
            assistant_message("code", "gpt-5-mini"),
            Event::EdgeSelected {
                from_node:          "code".into(),
                to_node:            "review".into(),
                label:              Some("ship".into()),
                condition:          None,
                reason:             "condition".into(),
                preferred_label:    None,
                suggested_next_ids: Vec::new(),
                stage_status:       "success".into(),
                is_jump:            false,
            },
            Event::StageRetrying {
                node_id:      "code".into(),
                name:         "Code".into(),
                index:        0,
                attempt:      2,
                max_attempts: 3,
                delay_ms:     1500,
            },
            agent_event("code", AgentEvent::Warning {
                kind:    "context_window".into(),
                message: "high usage".into(),
                details: serde_json::json!({"usage_percent": 92}),
            }),
            agent_event("code", AgentEvent::LlmRetry {
                provider:   "openai".into(),
                model:      "gpt-5-mini".into(),
                attempt:    2,
                delay_secs: 1.5,
                error:      fabro_llm::Error::Configuration {
                    message: "busy".into(),
                    source:  None,
                },
            }),
            agent_event("code", AgentEvent::SubAgentSpawned {
                agent_id: "a1".into(),
                depth:    1,
                task:     "review recent changes".into(),
            }),
            agent_event("code", AgentEvent::SubAgentCompleted {
                agent_id:   "a1".into(),
                depth:      1,
                success:    true,
                turns_used: 3,
            }),
            Event::SetupStarted { command_count: 1 },
            Event::SetupCommandCompleted {
                command:     "bun install".into(),
                index:       0,
                exit_code:   0,
                duration_ms: 2200,
            },
            Event::SetupCompleted { duration_ms: 2200 },
            Event::DevcontainerLifecycleStarted {
                phase:         "postCreate".into(),
                command_count: 1,
            },
            Event::DevcontainerLifecycleCommandCompleted {
                phase:       "postCreate".into(),
                command:     "npm run setup".into(),
                index:       0,
                exit_code:   0,
                duration_ms: 1400,
            },
            Event::DevcontainerLifecycleCompleted {
                phase:       "postCreate".into(),
                duration_ms: 1400,
            },
        ];

        let (mut event_ui, event_buffer) = capture_ui(true);
        for event in &events {
            emit_ref(&mut event_ui, event);
        }

        let (mut json_ui, json_buffer) = capture_ui(true);
        for event in &events {
            let line = serde_json::to_string(&to_run_event(&fixtures::RUN_1, event)).unwrap();
            json_ui.handle_json_line(&line);
        }

        assert_eq!(rendered(&event_buffer), rendered(&json_buffer));
    }

    #[test]
    fn plain_default_stage_snapshot() {
        let (mut ui, buffer) = capture_ui(false);

        emit(&mut ui, stage_started("plan", "Plan"));
        emit(&mut ui, assistant_message("plan", "gpt-5-mini"));
        emit(
            &mut ui,
            agent_event("plan", AgentEvent::ToolCallStarted {
                tool_name:    "read_file".into(),
                tool_call_id: "tc1".into(),
                arguments:    serde_json::json!({"path": "src/main.rs"}),
            }),
        );
        emit(
            &mut ui,
            agent_event("plan", AgentEvent::ToolCallCompleted {
                tool_name:    "read_file".into(),
                tool_call_id: "tc1".into(),
                output:       serde_json::json!({"ok": true}),
                is_error:     false,
            }),
        );
        emit(&mut ui, stage_completed("plan", "Plan"));

        insta::assert_snapshot!(rendered(&buffer), @"    ✓ Plan  $0.00   5s");
    }

    #[test]
    fn plain_default_setup_snapshot() {
        let (mut ui, buffer) = capture_ui(false);

        emit(&mut ui, Event::Sandbox {
            event: SandboxEvent::Initializing {
                provider: "daytona".into(),
            },
        });
        emit(&mut ui, Event::Sandbox {
            event: SandboxEvent::Ready {
                provider:    "daytona".into(),
                duration_ms: 2500,
                name:        Some("sandbox-1".into()),
                cpu:         Some(4.0),
                memory:      Some(8.0),
                url:         None,
            },
        });
        emit(&mut ui, Event::SshAccessReady {
            ssh_command: "ssh daytona@example".into(),
        });
        emit(&mut ui, Event::SetupStarted { command_count: 2 });
        emit(&mut ui, Event::SetupCompleted { duration_ms: 8200 });
        emit(&mut ui, Event::CliEnsureCompleted {
            cli_name:          "gh".into(),
            provider:          "github".into(),
            already_installed: false,
            node_installed:    false,
            duration_ms:       600,
        });
        emit(&mut ui, Event::DevcontainerResolved {
            dockerfile_lines:        24,
            environment_count:       3,
            lifecycle_command_count: 2,
            workspace_folder:        "/workspace".into(),
        });
        emit(&mut ui, Event::DevcontainerLifecycleStarted {
            phase:         "postCreate".into(),
            command_count: 2,
        });
        emit(&mut ui, Event::DevcontainerLifecycleCompleted {
            phase:       "postCreate".into(),
            duration_ms: 1800,
        });

        insta::assert_snapshot!(rendered(&buffer), @r"
            Sandbox: daytona (ready in 2s)
                     sandbox-1 (4 cpu, 8 GB)
                     ssh daytona@example
            Setup: 2 commands (8s)
            CLI: gh (installed, 600ms)
            Devcontainer: resolved
                     24 Dockerfile lines, 3 env vars, 2 lifecycle cmds, /workspace
            Running devcontainer postCreate (2 commands)...
            Devcontainer: postCreate (1s)
        ");
    }

    #[test]
    fn plain_verbose_snapshot() {
        let (mut ui, buffer) = capture_ui(true);

        emit(&mut ui, stage_started("code", "Code"));
        emit(&mut ui, Event::SandboxInitialized {
            working_directory: "/home/daytona/workspace".into(),
            provider:          "daytona".into(),
            identifier:        None,
            repo_cloned:       None,
            clone_origin_url:  None,
            clone_branch:      None,
        });
        emit(
            &mut ui,
            agent_event("code", AgentEvent::ToolCallStarted {
                tool_name:    "read_file".into(),
                tool_call_id: "tc1".into(),
                arguments:    serde_json::json!({
                    "file_path": "/home/daytona/workspace/src/main.rs"
                }),
            }),
        );
        emit(&mut ui, assistant_message("code", "gpt-5-mini"));
        emit(&mut ui, Event::EdgeSelected {
            from_node:          "code".into(),
            to_node:            "review".into(),
            label:              Some("ship".into()),
            condition:          None,
            reason:             "condition".into(),
            preferred_label:    None,
            suggested_next_ids: Vec::new(),
            stage_status:       "success".into(),
            is_jump:            false,
        });
        emit(&mut ui, Event::StageRetrying {
            node_id:      "code".into(),
            name:         "Code".into(),
            index:        0,
            attempt:      2,
            max_attempts: 3,
            delay_ms:     1500,
        });
        emit(
            &mut ui,
            agent_event("code", AgentEvent::Warning {
                kind:    "context_window".into(),
                message: "high usage".into(),
                details: serde_json::json!({"usage_percent": 92}),
            }),
        );
        emit(
            &mut ui,
            agent_event("code", AgentEvent::LlmRetry {
                provider:   "openai".into(),
                model:      "gpt-5-mini".into(),
                attempt:    2,
                delay_secs: 1.5,
                error:      fabro_llm::Error::Configuration {
                    message: "busy".into(),
                    source:  None,
                },
            }),
        );
        emit(
            &mut ui,
            agent_event("code", AgentEvent::SubAgentSpawned {
                agent_id: "a1".into(),
                depth:    1,
                task:     "review recent changes".into(),
            }),
        );
        emit(
            &mut ui,
            agent_event("code", AgentEvent::SubAgentCompleted {
                agent_id:   "a1".into(),
                depth:      1,
                success:    true,
                turns_used: 3,
            }),
        );
        emit(&mut ui, Event::SetupStarted { command_count: 1 });
        emit(&mut ui, Event::SetupCommandCompleted {
            command:     "bun install".into(),
            index:       0,
            exit_code:   0,
            duration_ms: 2200,
        });
        emit(&mut ui, Event::SetupCompleted { duration_ms: 2200 });
        emit(&mut ui, Event::DevcontainerLifecycleStarted {
            phase:         "postCreate".into(),
            command_count: 1,
        });
        emit(&mut ui, Event::DevcontainerLifecycleCommandCompleted {
            phase:       "postCreate".into(),
            command:     "npm run setup".into(),
            index:       0,
            exit_code:   0,
            duration_ms: 1400,
        });
        emit(&mut ui, Event::DevcontainerLifecycleCompleted {
            phase:       "postCreate".into(),
            duration_ms: 1400,
        });
        emit(&mut ui, stage_completed("code", "Code"));

        insta::assert_snapshot!(rendered(&buffer), @r#"
        → code → review  "ship"
        ↻ Code: retrying (attempt 2/3, delay 1s)
          ⚠ context window: 92% used
          ⚠ retry: gpt-5-mini attempt 2 (busy, delay 1s)
            ▸ subagent[a1] "review recent changes"
            ✓ subagent[a1] (3 turns)
          ✓ [1/1] bun install  2s
        Setup: 1 command (2s)
        Running devcontainer postCreate (1 commands)...
          ✓ [1/1] npm run setup  1s
        Devcontainer: postCreate (1s)
        ✓ Code  $0.00   5s  (1 turns, 0 tools, 1.5k toks)
        "#);
    }

    #[test]
    fn plain_notice_snapshot() {
        let (mut ui, buffer) = capture_ui(false);

        emit(&mut ui, Event::RunNotice {
            level:   RunNoticeLevel::Warn,
            code:    "sandbox_cleanup_failed".into(),
            message: "sandbox cleanup failed".into(),
        });
        emit(&mut ui, Event::PullRequestCreated {
            pr_url:      "https://github.com/fabro-sh/fabro/pull/42".into(),
            pr_number:   42,
            owner:       "fabro-sh".into(),
            repo:        "fabro".into(),
            base_branch: "main".into(),
            head_branch: "fabro/run/42".into(),
            title:       "Ship the change".into(),
            draft:       true,
        });
        emit(&mut ui, Event::PullRequestFailed {
            error: "auth token expired".into(),
        });

        insta::assert_snapshot!(rendered(&buffer), @r"
            Warning: sandbox cleanup failed [sandbox_cleanup_failed]
            Draft PR: https://github.com/fabro-sh/fabro/pull/42
            PR failed: auth token expired
        ");
    }

    #[test]
    fn plain_metadata_snapshot_snapshot() {
        let (mut ui, buffer) = capture_ui(false);

        emit(&mut ui, Event::MetadataSnapshotCompleted {
            phase:       MetadataSnapshotPhase::Checkpoint,
            branch:      "fabro/meta".into(),
            duration_ms: 2000,
            entry_count: 2,
            bytes:       42,
            commit_sha:  "abc123".into(),
        });
        emit(&mut ui, Event::MetadataSnapshotFailed {
            phase:        MetadataSnapshotPhase::Finalize,
            branch:       "fabro/meta".into(),
            duration_ms:  900,
            failure_kind: MetadataSnapshotFailureKind::Push,
            error:        "push rejected".into(),
            causes:       Vec::new(),
            commit_sha:   Some("abc123".into()),
            entry_count:  Some(2),
            bytes:        Some(42),
        });

        insta::assert_snapshot!(rendered(&buffer), @r"
            Metadata checkpoint 2s
            Warning: Metadata finalize failed: push rejected [push]
        ");
    }

    #[test]
    fn metadata_snapshot_failure_suppresses_compat_notice_only() {
        let (mut ui, buffer) = capture_ui(false);

        emit(&mut ui, Event::MetadataSnapshotFailed {
            phase:        MetadataSnapshotPhase::Checkpoint,
            branch:       "fabro/meta".into(),
            duration_ms:  900,
            failure_kind: MetadataSnapshotFailureKind::Write,
            error:        "write failed".into(),
            causes:       Vec::new(),
            commit_sha:   None,
            entry_count:  None,
            bytes:        None,
        });
        emit(&mut ui, Event::RunNotice {
            level:   RunNoticeLevel::Warn,
            code:    "checkpoint_metadata_write_failed".into(),
            message: "legacy metadata warning".into(),
        });
        emit(&mut ui, Event::RunNotice {
            level:   RunNoticeLevel::Warn,
            code:    "checkpoint_metadata_degraded".into(),
            message: "metadata snapshots are disabled for this run".into(),
        });

        insta::assert_snapshot!(rendered(&buffer), @r"
            Warning: Metadata checkpoint failed: write failed [write]
            Warning: metadata snapshots are disabled for this run [checkpoint_metadata_degraded]
        ");
    }

    #[test]
    fn tty_parallel_branch_completion_uses_recorded_duration() {
        let mut ui = ProgressUI::new(true, false);

        emit(&mut ui, stage_started("fork1", "Fork"));
        emit(&mut ui, Event::ParallelStarted {
            node_id:      "fork1".into(),
            visit:        1,
            branch_count: 1,
            join_policy:  "wait_all".into(),
        });
        emit(&mut ui, Event::ParallelBranchStarted {
            parallel_group_id:  StageId::new("fork1", 1),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fork1", 1), 0),
            branch:             "security".into(),
            index:              0,
        });
        emit(&mut ui, Event::ParallelBranchCompleted {
            parallel_group_id:  StageId::new("fork1", 1),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fork1", 1), 0),
            branch:             "security".into(),
            index:              0,
            duration_ms:        500,
            status:             "success".into(),
            head_sha:           None,
        });

        let stage = &ui.stage.active_stages["fork1"];
        assert_eq!(stage.tool_calls[0].bar.prefix(), "500ms");
    }

    #[test]
    fn tty_tool_call_completion_uses_jsonl_timestamps() {
        let mut ui = ProgressUI::new(true, false);

        let started_ts = DateTime::parse_from_rfc3339("2026-03-30T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);
        let completed_ts = DateTime::parse_from_rfc3339("2026-03-30T12:00:00.500Z")
            .unwrap()
            .with_timezone(&Utc);

        let stage_started = serde_json::to_string(&to_run_event_at(
            &fixtures::RUN_1,
            &Event::StageStarted {
                node_id:      "code".into(),
                name:         "Code".into(),
                index:        0,
                handler_type: "agent".into(),
                attempt:      1,
                max_attempts: 1,
            },
            started_ts,
            None,
        ))
        .unwrap();
        let tool_started = serde_json::to_string(&to_run_event_at(
            &fixtures::RUN_1,
            &agent_event("code", AgentEvent::ToolCallStarted {
                tool_name:    "read_file".into(),
                tool_call_id: "tc1".into(),
                arguments:    serde_json::json!({"path": "src/main.rs"}),
            }),
            started_ts,
            None,
        ))
        .unwrap();
        let tool_completed = serde_json::to_string(&to_run_event_at(
            &fixtures::RUN_1,
            &agent_event("code", AgentEvent::ToolCallCompleted {
                tool_name:    "read_file".into(),
                tool_call_id: "tc1".into(),
                output:       serde_json::json!({"ok": true}),
                is_error:     false,
            }),
            completed_ts,
            None,
        ))
        .unwrap();

        ui.handle_json_line(&stage_started);
        ui.handle_json_line(&tool_started);
        ui.handle_json_line(&tool_completed);

        let stage = &ui.stage.active_stages["code"];
        assert_eq!(stage.tool_calls[0].bar.prefix(), "500ms");
    }
}
