#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI run-progress renderer: writes to std::io::stderr directly"
)]

use fabro_types::{RunNoticeCode, RunStreamItem};

mod event;
mod info_display;
mod petri;
mod renderer;
mod stage_display;
mod styles;

use event::ProgressEvent;
use info_display::InfoDisplay;
use petri::PetriProgressState;
use renderer::ProgressRenderer;
use stage_display::StageDisplay;

pub(crate) struct ProgressUI {
    renderer: ProgressRenderer,
    stage: StageDisplay,
    info: InfoDisplay,
    saw_metadata_snapshot_failure: bool,
    petri: PetriProgressState,
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
            info: InfoDisplay::new(verbose),
            saw_metadata_snapshot_failure: false,
            petri: PetriProgressState::default(),
        }
    }

    pub(crate) fn hide_bars(&self) {
        self.renderer.hide();
    }

    pub(crate) fn show_bars(&self) {
        self.renderer.show();
    }

    pub(crate) fn finish(&mut self) {
        self.stage.finish();
        self.renderer.finish();
    }

    /// One item of a Petri run's stream: the progress lines it means, if
    /// any.
    pub(crate) fn handle_stream_item(&mut self, item: &RunStreamItem) {
        for progress_event in petri::progress_events(item, &mut self.petri) {
            self.dispatch(progress_event);
        }
    }

    fn dispatch(&mut self, event: ProgressEvent) {
        let renderer = &self.renderer;
        match event {
            ProgressEvent::RunCreated { web_url } => {
                if let Some(url) = web_url {
                    InfoDisplay::show_web_url(renderer, &url);
                }
            }
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
                timing,
                status,
                usage,
            } => {
                self.stage.on_stage_completed(
                    renderer,
                    &node_id,
                    &name,
                    timing.wall_time_ms,
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
                    .on_parallel_branch_completed(renderer, &branch, duration_ms, status);
            }
            ProgressEvent::ParallelCompleted => {
                self.stage.on_parallel_completed();
            }
            ProgressEvent::AssistantMessage {
                stage_node_id,
                model,
                root_session,
            } => {
                if root_session {
                    self.stage.on_llm_request_finished(&stage_node_id);
                }
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
            ProgressEvent::CompactionFailed {
                stage_node_id,
                error,
                root_session,
            } => {
                if root_session {
                    self.stage.on_llm_request_finished(&stage_node_id);
                }
                self.stage
                    .on_compaction_failed(renderer, &stage_node_id, &error);
            }
            ProgressEvent::LlmRequestStarted {
                stage_node_id,
                model,
            } => {
                self.stage
                    .on_llm_request_started(renderer, &stage_node_id, &model);
            }
            ProgressEvent::LlmFirstOutput {
                stage_node_id,
                kind,
            } => {
                self.stage.on_llm_first_output(&stage_node_id, kind);
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
            ProgressEvent::LlmRequestFinished { stage_node_id } => {
                self.stage.on_llm_request_finished(&stage_node_id);
            }
            ProgressEvent::SubagentStarted {
                stage_node_id,
                agent_id,
                task,
                generation,
            } => {
                self.stage.on_subagent_started(
                    renderer,
                    &stage_node_id,
                    &agent_id,
                    &task,
                    generation,
                );
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
            ProgressEvent::RunNotice {
                level,
                code,
                message,
            } => {
                if self.saw_metadata_snapshot_failure
                    && code
                        .parse::<RunNoticeCode>()
                        .ok()
                        .is_some_and(RunNoticeCode::is_metadata_snapshot_compat)
                {
                    return;
                }
                InfoDisplay::on_run_notice(renderer, level, &code, &message);
            }
            ProgressEvent::PullRequestCreated { pr_url, draft } => {
                InfoDisplay::on_pull_request_created(renderer, &pr_url, draft);
            }
        }
    }
}
