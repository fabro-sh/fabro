use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use console::Style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use fabro_agent::AgentEvent;
use fabro_interview::{Answer, ConsoleInterviewer, Interviewer, Question};
use fabro_workflows::event::{EventEmitter, RunNoticeLevel, WorkflowRunEvent};
use fabro_workflows::outcome::StageStatus;

use crate::shared::{format_duration_ms, format_tokens_human, tilde_path};
use fabro_workflows::outcome::{compute_stage_cost, format_cost};

// ── Cached styles ───────────────────────────────────────────────────────

macro_rules! cached_style {
    ($name:ident, $template:expr) => {
        fn $name() -> ProgressStyle {
            static STYLE: OnceLock<ProgressStyle> = OnceLock::new();
            STYLE
                .get_or_init(|| ProgressStyle::with_template($template).expect("valid template"))
                .clone()
        }
    };
}

cached_style!(
    style_header_running,
    "    {spinner:.dim} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_header_done, "    {wide_msg:.dim} {prefix:.dim}");
cached_style!(
    style_stage_running,
    "    {spinner:.cyan} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_stage_done, "    {wide_msg} {prefix:.dim}");
cached_style!(
    style_tool_running,
    "      {spinner:.dim} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_tool_done, "      {wide_msg} {prefix:.dim}");
cached_style!(style_subagent_info, "        {wide_msg}");
cached_style!(style_branch_done, "        {wide_msg} {prefix:.dim}");
cached_style!(style_static_dim, "    {wide_msg:.dim}");
cached_style!(style_sandbox_detail, "             {wide_msg:.dim}");
cached_style!(style_empty, " ");

// ── Cached glyphs ───────────────────────────────────────────────────────

fn green_check() -> &'static str {
    static GLYPH: OnceLock<String> = OnceLock::new();
    GLYPH.get_or_init(|| Style::new().green().apply_to("\u{2713}").to_string())
}

fn red_cross() -> &'static str {
    static GLYPH: OnceLock<String> = OnceLock::new();
    GLYPH.get_or_init(|| Style::new().red().apply_to("\u{2717}").to_string())
}

// ── Duration formatting ─────────────────────────────────────────────────

pub(crate) fn format_duration_short(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if d.as_millis() >= 1000 {
        format!("{}s", secs)
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// Wrap `text` in an OSC 8 terminal hyperlink pointing to `url`.
fn terminal_hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Format a number as an integer if whole, one decimal otherwise.
fn format_number(n: f64) -> String {
    if (n - n.round()).abs() < f64::EPSILON {
        format!("{}", n as i64)
    } else {
        format!("{n:.1}")
    }
}

// ── Tool call display name ──────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    let single_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.len() > max {
        let mut t: String = single_line.chars().take(max - 3).collect();
        t.push_str("...");
        t
    } else {
        single_line
    }
}

fn last_line_truncated(s: &str, max: usize) -> String {
    let line = s
        .trim()
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.len() > max {
        let mut t: String = line.chars().take(max - 3).collect();
        t.push_str("...");
        t
    } else {
        line.to_string()
    }
}

fn shorten_path(path: &str, working_directory: Option<&str>) -> String {
    if let Some(wd) = working_directory {
        if let Ok(rel) = std::path::Path::new(path).strip_prefix(wd) {
            return rel.display().to_string();
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = std::path::Path::new(path).strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    path.to_string()
}

// ── Tool call entry ─────────────────────────────────────────────────────

enum ToolCallStatus {
    Running,
    Succeeded,
    Failed,
}

struct ToolCallEntry {
    display_name: String,
    tool_call_id: String,
    status: ToolCallStatus,
    bar: ProgressBar,
    is_branch: bool,
}

// ── Active stage ────────────────────────────────────────────────────────

struct ActiveStage {
    display_name: String,
    has_model: bool,
    spinner: ProgressBar,
    tool_calls: VecDeque<ToolCallEntry>,
    compaction_bar: Option<ProgressBar>,
}

impl ActiveStage {
    fn last_bar(&self) -> &ProgressBar {
        self.tool_calls.back().map_or(&self.spinner, |e| &e.bar)
    }
}

const MAX_TOOL_CALLS: usize = 5;

// ── Renderer variants ───────────────────────────────────────────────────

struct TtyRenderer {
    multi: MultiProgress,
}

enum ProgressRenderer {
    Tty(TtyRenderer),
    Plain,
}

// ── ProgressUI ──────────────────────────────────────────────────────────

pub struct ProgressUI {
    renderer: ProgressRenderer,
    verbose: bool,
    active_stages: HashMap<String, ActiveStage>,
    /// Turn and tool-call counts per stage, tracked independently of the
    /// renderer so that Plain (non-TTY) mode reports accurate stats.
    stage_counts: HashMap<String, (u32, u32)>,
    setup_command_count: usize,
    devcontainer_command_count: usize,
    sandbox_bar: Option<ProgressBar>,
    setup_bar: Option<ProgressBar>,
    devcontainer_bar: Option<ProgressBar>,
    cli_ensure_bar: Option<ProgressBar>,
    any_stage_started: bool,
    parallel_parent: Option<String>,
    working_directory: Option<String>,
}

impl ProgressUI {
    pub fn new(is_tty: bool, verbose: bool) -> Self {
        let renderer = if is_tty {
            ProgressRenderer::Tty(TtyRenderer {
                multi: MultiProgress::new(),
            })
        } else {
            ProgressRenderer::Plain
        };
        Self {
            renderer,
            verbose,
            active_stages: HashMap::new(),
            stage_counts: HashMap::new(),
            setup_command_count: 0,
            devcontainer_command_count: 0,
            sandbox_bar: None,
            setup_bar: None,
            devcontainer_bar: None,
            cli_ensure_bar: None,
            any_stage_started: false,
            parallel_parent: None,
            working_directory: None,
        }
    }

    pub fn set_working_directory(&mut self, dir: String) {
        self.working_directory = Some(dir);
    }

    fn tool_display_name(&self, tool_name: &str, arguments: &serde_json::Value) -> String {
        let dim = Style::new().dim();
        let arg = |key: &str| arguments.get(key).and_then(|v| v.as_str());
        let wd = self.working_directory.as_deref();
        let path_arg = || {
            arg("path")
                .or_else(|| arg("file_path"))
                .map(|p| truncate(&shorten_path(p, wd), 60))
        };

        let detail = match tool_name {
            "bash" | "shell" | "execute_command" => arg("command").map(|c| truncate(c, 60)),
            "glob" => arg("pattern").map(String::from),
            "grep" | "ripgrep" => arg("pattern").map(|p| truncate(p, 40)),
            "read_file" | "read" => path_arg(),
            "write_file" | "write" | "create_file" => path_arg(),
            "edit_file" | "edit" => path_arg(),
            "list_dir" => path_arg(),
            "web_search" => arg("query").map(|q| truncate(q, 60)),
            "web_fetch" => arg("url").map(|u| truncate(u, 60)),
            "spawn_agent" => arg("task").map(|t| truncate(t, 60)),
            "wait" | "send_input" | "close_agent" => arg("agent_id").map(String::from),
            "use_skill" => arg("skill_name").map(String::from),
            "apply_patch" => Some("…".into()),
            "read_many_files" => arguments
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| format!("{} files", a.len())),
            _ => None,
        };

        match detail {
            Some(d) => format!("{tool_name}{}", dim.apply_to(format!("({d})"))),
            None => tool_name.to_string(),
        }
    }

    /// Register event handlers on the emitter.
    pub fn register(progress: &Arc<Mutex<Self>>, emitter: &EventEmitter) {
        let p = Arc::clone(progress);
        emitter.on_event(move |event| {
            let mut ui = p.lock().expect("progress lock poisoned");
            ui.handle_event(event);
        });
    }

    /// Hide indicatif progress bars (for interview prompts in attach mode).
    pub fn hide_bars(&self) {
        if let ProgressRenderer::Tty(tty) = &self.renderer {
            tty.multi.set_draw_target(ProgressDrawTarget::hidden());
        }
    }

    /// Show indicatif progress bars after an interview prompt.
    pub fn show_bars(&self) {
        if let ProgressRenderer::Tty(tty) = &self.renderer {
            tty.multi.set_draw_target(ProgressDrawTarget::stderr());
        }
    }

    /// Clear all active bars and release the terminal for normal stderr output.
    pub fn finish(&mut self) {
        for (_id, stage) in self.active_stages.drain() {
            for entry in &stage.tool_calls {
                if entry.is_branch || self.verbose {
                    entry.bar.abandon();
                } else {
                    entry.bar.finish_and_clear();
                }
            }
            stage.spinner.finish_and_clear();
        }
        if let ProgressRenderer::Tty(tty) = &self.renderer {
            // Add a trailing blank line through indicatif so it survives the final redraw
            let sep = tty.multi.add(ProgressBar::new_spinner());
            sep.set_style(style_empty());
            sep.finish();
            tty.multi.set_draw_target(ProgressDrawTarget::hidden());
        }
    }

    // ── Event dispatch ──────────────────────────────────────────────────

    pub(crate) fn handle_event(&mut self, event: &WorkflowRunEvent) {
        match event {
            WorkflowRunEvent::WorkflowRunStarted {
                base_branch,
                base_sha,
                worktree_dir,
                ..
            } => {
                if let Some(worktree_dir) = worktree_dir {
                    self.show_worktree(std::path::Path::new(worktree_dir));
                }
                if let Some(base_sha) = base_sha {
                    self.show_base_info(base_branch.as_deref(), base_sha);
                }
            }
            WorkflowRunEvent::Sandbox {
                event: sandbox_event,
            } => {
                self.on_sandbox_event(sandbox_event);
            }
            WorkflowRunEvent::SetupStarted { command_count } => {
                self.on_setup_started(*command_count);
            }
            WorkflowRunEvent::SetupCompleted { duration_ms } => {
                self.on_setup_completed(*duration_ms);
            }
            WorkflowRunEvent::StageStarted {
                node_id,
                name,
                script,
                ..
            } => {
                self.on_stage_started(node_id, name, script.as_deref());
            }
            WorkflowRunEvent::StageCompleted {
                node_id,
                name,
                duration_ms,
                status,
                usage,
                ..
            } => {
                let succeeded = status
                    .parse::<StageStatus>()
                    .map(|s| matches!(s, StageStatus::Success | StageStatus::PartialSuccess))
                    .unwrap_or(false);
                let dur = format_duration_ms(*duration_ms);
                let cost_str = usage
                    .as_ref()
                    .and_then(compute_stage_cost)
                    .map(|c| format!("{}   ", format_cost(c)))
                    .unwrap_or_default();
                let stats_str = if self.verbose {
                    let counts = self.stage_counts.get(node_id);
                    let turn_count = counts.map_or(0, |c| c.0);
                    let tool_call_count = counts.map_or(0, |c| c.1);
                    let total_tokens = usage
                        .as_ref()
                        .map(|u| u.input_tokens + u.output_tokens)
                        .unwrap_or(0);
                    if turn_count > 0 || tool_call_count > 0 || total_tokens > 0 {
                        let dim = Style::new().dim();
                        format!(
                            "  {}",
                            dim.apply_to(format!(
                                "({} turns, {} tools, {} toks)",
                                turn_count,
                                tool_call_count,
                                format_tokens_human(total_tokens),
                            ))
                        )
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                let prefix = format!("{cost_str}{dur}{stats_str}");
                let glyph = if succeeded {
                    green_check()
                } else {
                    red_cross()
                };
                self.finish_stage(node_id, name, glyph, &prefix);
            }
            WorkflowRunEvent::StageFailed {
                node_id,
                name,
                failure,
                ..
            } => {
                self.finish_stage(node_id, name, red_cross(), "");
                let red = Style::new().red();
                let summary = last_line_truncated(&failure.message, 120);
                self.insert_info_line(&format!("{} {}", red.apply_to("Error:"), summary));
            }
            WorkflowRunEvent::ParallelStarted { .. } => {
                // The fork stage is the (only) active stage at this point.
                // In Plain mode active_stages is empty, so use a sentinel.
                self.parallel_parent = self
                    .active_stages
                    .keys()
                    .next()
                    .cloned()
                    .or_else(|| Some(String::new()));
            }
            WorkflowRunEvent::ParallelBranchStarted { branch, .. } => {
                self.on_parallel_branch_started(branch);
            }
            WorkflowRunEvent::ParallelBranchCompleted {
                branch,
                duration_ms,
                status,
                ..
            } => {
                self.on_parallel_branch_completed(branch, *duration_ms, status);
            }
            WorkflowRunEvent::ParallelCompleted { .. } => {
                self.parallel_parent = None;
            }
            WorkflowRunEvent::Agent { stage, event } => {
                self.on_agent_event(stage, event);
            }
            WorkflowRunEvent::SshAccessReady { ssh_command } => {
                self.on_ssh_access_ready(ssh_command);
            }
            WorkflowRunEvent::EdgeSelected {
                from_node,
                to_node,
                label,
                condition,
                ..
            } if self.verbose => {
                let detail = if let Some(c) = condition {
                    format!("  [{c}]")
                } else if let Some(l) = label {
                    format!("  \"{l}\"")
                } else {
                    String::new()
                };
                self.insert_info_line(&format!("\u{2192} {from_node} \u{2192} {to_node}{detail}"));
            }
            WorkflowRunEvent::LoopRestart { from_node, to_node } if self.verbose => {
                self.insert_info_line(&format!(
                    "\u{21ba} {from_node} \u{2192} {to_node}  (loop restart)"
                ));
            }
            WorkflowRunEvent::SetupCommandCompleted {
                command,
                index,
                exit_code,
                duration_ms,
            } if self.verbose => {
                let total = self.setup_command_count;
                let dur = format_duration_ms(*duration_ms);
                let glyph = if *exit_code == 0 {
                    green_check()
                } else {
                    red_cross()
                };
                let msg = format!("{glyph} [{}/{total}] {}", index + 1, truncate(command, 60),);
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let bar = if let Some(ref setup_bar) = self.setup_bar {
                            tty.multi
                                .insert_before(setup_bar, ProgressBar::new_spinner())
                        } else {
                            tty.multi.add(ProgressBar::new_spinner())
                        };
                        bar.set_style(style_tool_done());
                        bar.set_prefix(dur);
                        bar.finish_with_message(msg);
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("      {msg}  {dur}");
                    }
                }
            }
            WorkflowRunEvent::StageRetrying {
                node_id: _,
                name,
                attempt,
                max_attempts,
                delay_ms,
                ..
            } if self.verbose => {
                let dur = format_duration_ms(*delay_ms);
                self.insert_info_line(&format!(
                    "\u{21bb} {name}: retrying (attempt {attempt}/{max_attempts}, delay {dur})"
                ));
            }
            WorkflowRunEvent::CliEnsureStarted { cli_name, .. } => {
                self.on_cli_ensure_started(cli_name);
            }
            WorkflowRunEvent::CliEnsureCompleted {
                cli_name,
                already_installed,
                duration_ms,
                ..
            } => {
                self.on_cli_ensure_completed(cli_name, *already_installed, *duration_ms);
            }
            WorkflowRunEvent::CliEnsureFailed { cli_name, .. } => {
                self.on_cli_ensure_failed(cli_name);
            }
            WorkflowRunEvent::DevcontainerResolved {
                dockerfile_lines,
                environment_count,
                lifecycle_command_count,
                workspace_folder,
            } => {
                let detail = format!(
                    "{dockerfile_lines} Dockerfile lines, {environment_count} env vars, \
                     {lifecycle_command_count} lifecycle cmds, {workspace_folder}"
                );
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let bar = tty.multi.add(ProgressBar::new_spinner());
                        bar.set_style(style_header_done());
                        bar.finish_with_message("Devcontainer: resolved".to_string());
                        let detail_bar = tty.multi.insert_after(&bar, ProgressBar::new_spinner());
                        detail_bar.set_style(style_sandbox_detail());
                        detail_bar.finish_with_message(detail);
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Devcontainer: resolved");
                        eprintln!("             {detail}");
                    }
                }
            }
            WorkflowRunEvent::DevcontainerLifecycleStarted {
                phase,
                command_count,
            } => {
                self.devcontainer_command_count = *command_count;
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let bar = tty.multi.add(ProgressBar::new_spinner());
                        bar.set_style(style_header_running());
                        bar.set_message(format!(
                            "Running devcontainer {phase} ({command_count} commands)..."
                        ));
                        bar.enable_steady_tick(Duration::from_millis(100));
                        self.devcontainer_bar = Some(bar);
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Running devcontainer {phase} ({command_count} commands)...");
                    }
                }
            }
            WorkflowRunEvent::DevcontainerLifecycleCompleted {
                phase, duration_ms, ..
            } => {
                let dur = format_duration_ms(*duration_ms);
                match &self.renderer {
                    ProgressRenderer::Tty(_) => {
                        if let Some(bar) = self.devcontainer_bar.take() {
                            bar.set_style(style_header_done());
                            bar.set_prefix(dur);
                            bar.finish_with_message(format!("Devcontainer: {phase}"));
                        }
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Devcontainer: {phase} ({dur})");
                    }
                }
            }
            WorkflowRunEvent::DevcontainerLifecycleFailed {
                phase,
                command,
                exit_code,
                stderr,
                ..
            } => {
                if let Some(bar) = self.devcontainer_bar.take() {
                    bar.abandon();
                }
                let red = console::Style::new().red();
                let summary = if stderr.len() > 120 {
                    &stderr[..120]
                } else {
                    stderr.as_str()
                };
                self.insert_info_line(&format!(
                    "{} Devcontainer {phase} command failed (exit {exit_code}): {command}\n         {summary}",
                    red.apply_to("Error:")
                ));
            }
            WorkflowRunEvent::DevcontainerLifecycleCommandCompleted {
                command,
                index,
                exit_code,
                duration_ms,
                ..
            } if self.verbose => {
                let total = self.devcontainer_command_count;
                let dur = format_duration_ms(*duration_ms);
                let glyph = if *exit_code == 0 {
                    green_check()
                } else {
                    red_cross()
                };
                let msg = format!("{glyph} [{}/{total}] {}", index + 1, truncate(command, 60),);
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let bar = if let Some(ref dc_bar) = self.devcontainer_bar {
                            tty.multi.insert_before(dc_bar, ProgressBar::new_spinner())
                        } else {
                            tty.multi.add(ProgressBar::new_spinner())
                        };
                        bar.set_style(style_tool_done());
                        bar.set_prefix(dur);
                        bar.finish_with_message(msg);
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("      {msg}  {dur}");
                    }
                }
            }
            WorkflowRunEvent::RetroStarted => {
                self.on_stage_started("retro", "Retro", None);
            }
            WorkflowRunEvent::RetroCompleted { duration_ms } => {
                let dur = format_duration_ms(*duration_ms);
                self.finish_stage("retro", "Retro", green_check(), &dur);
            }
            WorkflowRunEvent::RetroFailed { duration_ms, .. } => {
                let dur = format_duration_ms(*duration_ms);
                self.finish_stage("retro", "Retro", red_cross(), &dur);
            }
            WorkflowRunEvent::RunNotice {
                level,
                code,
                message,
            } => {
                self.on_run_notice(*level, code, message);
            }
            WorkflowRunEvent::PullRequestCreated { pr_url, draft, .. } => {
                self.on_pull_request_created(pr_url, *draft);
            }
            WorkflowRunEvent::PullRequestFailed { error } => {
                self.on_pull_request_failed(error);
            }
            _ => {}
        }
    }

    // ── JSONL dispatch ────────────────────────────────────────────────

    /// Parse a JSONL envelope line and dispatch to internal rendering methods.
    /// Used by the attach loop to render events from progress.jsonl.
    pub fn handle_json_line(&mut self, line: &str) {
        let envelope: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return,
        };
        let event_name = match envelope.get("event").and_then(|v| v.as_str()) {
            Some(name) => name,
            None => return,
        };

        let str_field = |key: &str| -> Option<&str> { envelope.get(key).and_then(|v| v.as_str()) };
        let u64_field =
            |key: &str| -> u64 { envelope.get(key).and_then(|v| v.as_u64()).unwrap_or(0) };

        match event_name {
            "WorkflowRunStarted" => {
                if let Some(worktree_dir) = str_field("worktree_dir") {
                    self.show_worktree(std::path::Path::new(worktree_dir));
                }
                if let Some(base_sha) = str_field("base_sha") {
                    self.show_base_info(str_field("base_branch"), base_sha);
                }
            }
            "Sandbox.Initializing" => {
                let provider = str_field("sandbox_provider")
                    .unwrap_or("unknown")
                    .to_string();
                self.on_sandbox_event(&fabro_agent::SandboxEvent::Initializing { provider });
            }
            "Sandbox.Ready" => {
                let provider = str_field("sandbox_provider")
                    .unwrap_or("unknown")
                    .to_string();
                let duration_ms = u64_field("duration_ms");
                let name = str_field("name").map(String::from);
                let cpu = envelope.get("cpu").and_then(|v| v.as_f64());
                let memory = envelope.get("memory").and_then(|v| v.as_f64());
                let url = str_field("url").map(String::from);
                self.on_sandbox_event(&fabro_agent::SandboxEvent::Ready {
                    provider,
                    duration_ms,
                    name,
                    cpu,
                    memory,
                    url,
                });
            }
            "SandboxInitialized" => {
                if let Some(wd) = str_field("working_directory") {
                    self.set_working_directory(wd.to_string());
                }
            }
            "SetupStarted" => {
                let count = u64_field("command_count") as usize;
                self.on_setup_started(count);
            }
            "SetupCompleted" => {
                let duration_ms = u64_field("duration_ms");
                self.on_setup_completed(duration_ms);
            }
            "StageStarted" => {
                let node_id = str_field("node_id").unwrap_or("?");
                let name = str_field("node_label").unwrap_or("?");
                let script = str_field("script");
                self.on_stage_started(node_id, name, script);
            }
            "StageCompleted" => {
                let node_id = str_field("node_id").unwrap_or("?");
                let name = str_field("node_label").unwrap_or("?");
                let duration_ms = u64_field("duration_ms");
                let status = str_field("status").unwrap_or("success");
                let succeeded = matches!(status, "success" | "partial_success");

                let dur = format_duration_ms(duration_ms);

                // Parse usage for cost
                let cost_str = envelope
                    .get("usage")
                    .and_then(|u| u.get("cost"))
                    .and_then(|c| c.as_f64())
                    .map(|c| format!("{}   ", format_cost(c)))
                    .unwrap_or_default();

                let stats_str = if self.verbose {
                    let counts = self.stage_counts.get(node_id);
                    let turn_count = counts.map_or(0, |c| c.0);
                    let tool_call_count = counts.map_or(0, |c| c.1);
                    let total_tokens = envelope
                        .get("usage")
                        .map(|u| {
                            u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0)
                                + u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0)
                        })
                        .unwrap_or(0);
                    if turn_count > 0 || tool_call_count > 0 || total_tokens > 0 {
                        let dim = Style::new().dim();
                        format!(
                            "  {}",
                            dim.apply_to(format!(
                                "({} turns, {} tools, {} toks)",
                                turn_count,
                                tool_call_count,
                                format_tokens_human(total_tokens),
                            ))
                        )
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                let prefix = format!("{cost_str}{dur}{stats_str}");
                let glyph = if succeeded {
                    green_check()
                } else {
                    red_cross()
                };
                self.finish_stage(node_id, name, glyph, &prefix);
            }
            "StageFailed" => {
                let node_id = str_field("node_id").unwrap_or("?");
                let name = str_field("node_label").unwrap_or("?");
                let message = str_field("error")
                    .or_else(|| str_field("failure_reason"))
                    .unwrap_or("unknown error");
                self.finish_stage(node_id, name, red_cross(), "");
                let red = Style::new().red();
                let summary = last_line_truncated(message, 120);
                self.insert_info_line(&format!("{} {}", red.apply_to("Error:"), summary));
            }
            "ParallelStarted" => {
                self.parallel_parent = self
                    .active_stages
                    .keys()
                    .next()
                    .cloned()
                    .or_else(|| Some(String::new()));
            }
            "ParallelBranchStarted" => {
                if let Some(branch) = str_field("node_id") {
                    self.on_parallel_branch_started(branch);
                }
            }
            "ParallelBranchCompleted" => {
                if let Some(branch) = str_field("node_id") {
                    let duration_ms = u64_field("duration_ms");
                    let status = str_field("status").unwrap_or("success");
                    self.on_parallel_branch_completed(branch, duration_ms, status);
                }
            }
            "ParallelCompleted" => {
                self.parallel_parent = None;
            }
            "Agent.ToolCallStarted" => {
                let stage = str_field("node_id").unwrap_or("?");
                let tool_name = str_field("tool_name").unwrap_or("?");
                let tool_call_id = str_field("tool_call_id").unwrap_or("?");
                let empty = serde_json::Value::Object(serde_json::Map::new());
                let arguments = envelope.get("arguments").unwrap_or(&empty);
                // Update tool_call count
                if let Some(counts) = self.stage_counts.get_mut(stage) {
                    counts.1 += 1;
                }
                self.on_tool_call_started(stage, tool_name, tool_call_id, arguments);
            }
            "Agent.ToolCallCompleted" => {
                let stage = str_field("node_id").unwrap_or("?");
                let tool_call_id = str_field("tool_call_id").unwrap_or("?");
                let is_error = envelope
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.on_tool_call_completed(stage, tool_call_id, is_error);
            }
            "Agent.AssistantMessage" => {
                let stage = str_field("node_id").unwrap_or("?");
                let model = str_field("model").unwrap_or("?");
                // Update turn count
                if let Some(counts) = self.stage_counts.get_mut(stage) {
                    counts.0 += 1;
                }
                // Update model display on stage bar
                if let ProgressRenderer::Tty(_) = &self.renderer {
                    if let Some(active_stage) = self.active_stages.get_mut(stage) {
                        if !active_stage.has_model {
                            active_stage.has_model = true;
                            let dim = Style::new().dim();
                            let suffix = format!(" {}", dim.apply_to(format!("[{model}]")));
                            active_stage
                                .spinner
                                .set_message(format!("{}{}", active_stage.display_name, suffix));
                        }
                    }
                }
            }
            "Agent.CompactionStarted" => {
                let stage = str_field("node_id").unwrap_or("?");
                if let ProgressRenderer::Tty(tty) = &self.renderer {
                    if let Some(active_stage) = self.active_stages.get_mut(stage) {
                        if let Some(old) = active_stage.compaction_bar.take() {
                            old.finish_and_clear();
                        }
                        let bar = tty
                            .multi
                            .insert_after(active_stage.last_bar(), ProgressBar::new_spinner());
                        bar.set_style(style_tool_running());
                        bar.set_message("\u{27f3} compacting context\u{2026}");
                        bar.enable_steady_tick(Duration::from_millis(100));
                        active_stage.compaction_bar = Some(bar);
                    }
                }
            }
            "Agent.CompactionCompleted" => {
                let stage = str_field("node_id").unwrap_or("?");
                let original = u64_field("original_turn_count");
                let preserved = u64_field("preserved_turn_count");
                let tracked = u64_field("tracked_file_count");
                let msg = format!(
                    "\u{27f3} compaction: {original} \u{2192} {preserved} turns, {tracked} files"
                );
                match &self.renderer {
                    ProgressRenderer::Tty(_) => {
                        if let Some(bar) = self
                            .active_stages
                            .get_mut(stage)
                            .and_then(|s| s.compaction_bar.take())
                        {
                            bar.set_style(style_tool_done());
                            bar.finish_with_message(msg);
                        } else {
                            self.insert_info_line_for_stage(stage, &msg);
                        }
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("      {msg}");
                    }
                }
            }
            "SshAccessReady" => {
                if let Some(cmd) = str_field("ssh_command") {
                    self.on_ssh_access_ready(cmd);
                }
            }
            "RetroStarted" => {
                self.on_stage_started("retro", "Retro", None);
            }
            "RetroCompleted" => {
                let dur = format_duration_ms(u64_field("duration_ms"));
                self.finish_stage("retro", "Retro", green_check(), &dur);
            }
            "RetroFailed" => {
                let dur = format_duration_ms(u64_field("duration_ms"));
                self.finish_stage("retro", "Retro", red_cross(), &dur);
            }
            "RunNotice" => {
                let level = match str_field("level").unwrap_or("info") {
                    "warn" => RunNoticeLevel::Warn,
                    "error" => RunNoticeLevel::Error,
                    _ => RunNoticeLevel::Info,
                };
                let code = str_field("code").unwrap_or("");
                let message = str_field("message").unwrap_or("");
                self.on_run_notice(level, code, message);
            }
            "PullRequestCreated" => {
                let pr_url = str_field("pr_url").unwrap_or("?");
                let draft = envelope
                    .get("draft")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                self.on_pull_request_created(pr_url, draft);
            }
            "PullRequestFailed" => {
                let error = str_field("error").unwrap_or("unknown error");
                self.on_pull_request_failed(error);
            }
            "DevcontainerResolved" => {
                let dockerfile_lines = u64_field("dockerfile_lines");
                let environment_count = u64_field("environment_count");
                let lifecycle_command_count = u64_field("lifecycle_command_count");
                let workspace_folder = str_field("workspace_folder").unwrap_or("?").to_string();
                let detail = format!(
                    "{dockerfile_lines} Dockerfile lines, {environment_count} env vars, \
                     {lifecycle_command_count} lifecycle cmds, {workspace_folder}"
                );
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let bar = tty.multi.add(ProgressBar::new_spinner());
                        bar.set_style(style_header_done());
                        bar.finish_with_message("Devcontainer: resolved".to_string());
                        let detail_bar = tty.multi.insert_after(&bar, ProgressBar::new_spinner());
                        detail_bar.set_style(style_sandbox_detail());
                        detail_bar.finish_with_message(detail);
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Devcontainer: resolved");
                        eprintln!("             {detail}");
                    }
                }
            }
            "DevcontainerLifecycleStarted" => {
                let phase = str_field("phase").unwrap_or("?");
                let command_count = u64_field("command_count") as usize;
                self.devcontainer_command_count = command_count;
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let bar = tty.multi.add(ProgressBar::new_spinner());
                        bar.set_style(style_header_running());
                        bar.set_message(format!(
                            "Running devcontainer {phase} ({command_count} commands)..."
                        ));
                        bar.enable_steady_tick(Duration::from_millis(100));
                        self.devcontainer_bar = Some(bar);
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Running devcontainer {phase} ({command_count} commands)...");
                    }
                }
            }
            "DevcontainerLifecycleCompleted" => {
                let phase = str_field("phase").unwrap_or("?");
                let duration_ms = u64_field("duration_ms");
                let dur = format_duration_ms(duration_ms);
                match &self.renderer {
                    ProgressRenderer::Tty(_) => {
                        if let Some(bar) = self.devcontainer_bar.take() {
                            bar.set_style(style_header_done());
                            bar.set_prefix(dur);
                            bar.finish_with_message(format!("Devcontainer: {phase}"));
                        }
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Devcontainer: {phase} ({dur})");
                    }
                }
            }
            "DevcontainerLifecycleFailed" => {
                let phase = str_field("phase").unwrap_or("?");
                let command = str_field("command").unwrap_or("?");
                let exit_code = u64_field("exit_code");
                let stderr_text = str_field("stderr").unwrap_or("");
                if let Some(bar) = self.devcontainer_bar.take() {
                    bar.abandon();
                }
                let red = Style::new().red();
                let summary = if stderr_text.len() > 120 {
                    &stderr_text[..120]
                } else {
                    stderr_text
                };
                self.insert_info_line(&format!(
                    "{} Devcontainer {phase} command failed (exit {exit_code}): {command}\n         {summary}",
                    red.apply_to("Error:")
                ));
            }
            "CliEnsureStarted" => {
                if let Some(cli_name) = str_field("cli_name") {
                    self.on_cli_ensure_started(cli_name);
                }
            }
            "CliEnsureCompleted" => {
                if let Some(cli_name) = str_field("cli_name") {
                    let already_installed = envelope
                        .get("already_installed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let duration_ms = u64_field("duration_ms");
                    self.on_cli_ensure_completed(cli_name, already_installed, duration_ms);
                }
            }
            "CliEnsureFailed" => {
                if let Some(cli_name) = str_field("cli_name") {
                    self.on_cli_ensure_failed(cli_name);
                }
            }
            _ => {}
        }
    }

    // ── Sandbox ─────────────────────────────────────────────────────────

    fn on_sandbox_event(&mut self, event: &fabro_agent::SandboxEvent) {
        use fabro_agent::SandboxEvent;
        match event {
            SandboxEvent::Initializing { provider } => {
                if let ProgressRenderer::Tty(tty) = &self.renderer {
                    let bar = tty.multi.add(ProgressBar::new_spinner());
                    bar.set_style(style_header_running());
                    bar.set_message(format!("Initializing {provider} sandbox..."));
                    bar.enable_steady_tick(Duration::from_millis(100));
                    self.sandbox_bar = Some(bar);
                }
            }
            SandboxEvent::Ready {
                provider,
                duration_ms,
                name,
                cpu,
                memory,
                url,
            } => {
                let dur = format_duration_ms(*duration_ms);
                let detail = match (name, cpu, memory) {
                    (Some(n), Some(c), Some(m)) => Some(format!(
                        "{n} ({} cpu, {} GB)",
                        format_number(*c),
                        format_number(*m)
                    )),
                    (Some(n), _, _) => Some(n.clone()),
                    _ => None,
                };
                match &self.renderer {
                    ProgressRenderer::Tty(tty) => {
                        let display_provider = match url {
                            Some(u) => terminal_hyperlink(u, provider),
                            None => provider.clone(),
                        };
                        if let Some(bar) = self.sandbox_bar.take() {
                            bar.set_style(style_header_done());
                            bar.set_prefix(dur);
                            bar.finish_with_message(format!("Sandbox: {display_provider}"));
                            if let Some(detail_str) = &detail {
                                let detail_bar =
                                    tty.multi.insert_after(&bar, ProgressBar::new_spinner());
                                detail_bar.set_style(style_sandbox_detail());
                                detail_bar.finish_with_message(detail_str.clone());
                            }
                        }
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Sandbox: {provider} (ready in {dur})");
                        if let Some(detail_str) = &detail {
                            eprintln!("             {detail_str}");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ── SSH access ──────────────────────────────────────────────────────

    fn on_ssh_access_ready(&mut self, ssh_command: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_sandbox_detail());
                bar.finish_with_message(ssh_command.to_string());
            }
            ProgressRenderer::Plain => {
                eprintln!("             {ssh_command}");
            }
        }
    }

    // ── Setup ───────────────────────────────────────────────────────────

    fn on_setup_started(&mut self, command_count: usize) {
        self.setup_command_count = command_count;
        if let ProgressRenderer::Tty(tty) = &self.renderer {
            let bar = tty.multi.add(ProgressBar::new_spinner());
            bar.set_style(style_header_running());
            bar.set_message(format!(
                "Setup: {command_count} command{}...",
                if command_count == 1 { "" } else { "s" }
            ));
            bar.enable_steady_tick(Duration::from_millis(100));
            self.setup_bar = Some(bar);
        }
    }

    fn on_setup_completed(&mut self, duration_ms: u64) {
        let dur = format_duration_ms(duration_ms);
        let count = self.setup_command_count;
        let suffix = if count == 1 { "" } else { "s" };
        match &self.renderer {
            ProgressRenderer::Tty(_) => {
                if let Some(bar) = self.setup_bar.take() {
                    bar.set_style(style_header_done());
                    bar.set_prefix(dur);
                    bar.finish_with_message(format!("Setup: {count} command{suffix}"));
                }
            }
            ProgressRenderer::Plain => {
                eprintln!("    Setup: {count} command{suffix} ({dur})");
            }
        }
    }

    // ── CLI ensure ────────────────────────────────────────────────────────

    fn on_cli_ensure_started(&mut self, cli_name: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_header_running());
                bar.set_message(format!("CLI: ensuring {cli_name}..."));
                bar.enable_steady_tick(Duration::from_millis(100));
                self.cli_ensure_bar = Some(bar);
            }
            ProgressRenderer::Plain => {}
        }
    }

    fn on_cli_ensure_completed(
        &mut self,
        cli_name: &str,
        already_installed: bool,
        duration_ms: u64,
    ) {
        let dur = format_duration_ms(duration_ms);
        let status = if already_installed {
            "found"
        } else {
            "installed"
        };
        match &self.renderer {
            ProgressRenderer::Tty(_) => {
                if let Some(bar) = self.cli_ensure_bar.take() {
                    bar.set_style(style_header_done());
                    bar.set_prefix(dur);
                    bar.finish_with_message(format!("CLI: {cli_name} ({status})"));
                }
            }
            ProgressRenderer::Plain => {
                eprintln!("    CLI: {cli_name} ({status}, {dur})");
            }
        }
    }

    fn on_cli_ensure_failed(&mut self, cli_name: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(_) => {
                if let Some(bar) = self.cli_ensure_bar.take() {
                    bar.set_style(style_header_done());
                    bar.finish_with_message(format!(
                        "{} CLI: {cli_name} install failed",
                        red_cross()
                    ));
                }
            }
            ProgressRenderer::Plain => {
                eprintln!("    {} CLI: {cli_name} install failed", red_cross());
            }
        }
    }

    // ── Logs dir (called externally) ────────────────────────────────────

    pub fn show_run_dir(&mut self, run_dir: &Path) {
        let path_str = tilde_path(run_dir);
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(format!("Run:  {path_str}"));
            }
            ProgressRenderer::Plain => {
                eprintln!("    Run:  {path_str}");
            }
        }
    }

    pub fn show_version(&mut self) {
        let version = fabro_util::version::FABRO_VERSION;
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(format!("Version: {version}"));
            }
            ProgressRenderer::Plain => {
                eprintln!("    Version: {version}");
            }
        }
    }

    pub fn show_run_id(&mut self, run_id: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(format!("Run: {run_id}"));
            }
            ProgressRenderer::Plain => {
                eprintln!("    Run: {run_id}");
            }
        }
    }

    pub fn show_time(&mut self, time: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(format!("Time: {time}"));
            }
            ProgressRenderer::Plain => {
                eprintln!("    Time: {time}");
            }
        }
    }

    pub fn show_worktree(&mut self, path: &Path) {
        let path_str = tilde_path(path);
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(format!("Worktree: {path_str}"));
            }
            ProgressRenderer::Plain => {
                eprintln!("    Worktree: {path_str}");
            }
        }
    }

    pub fn show_base_info(&mut self, branch: Option<&str>, sha: &str) {
        let short_sha = &sha[..sha.len().min(12)];
        let text = match branch {
            Some(b) => format!("Base: {b} ({short_sha})"),
            None => format!("Base: {short_sha}"),
        };
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(text);
            }
            ProgressRenderer::Plain => {
                eprintln!("    {text}");
            }
        }
    }

    // ── Stages ──────────────────────────────────────────────────────────

    fn on_stage_started(&mut self, node_id: &str, name: &str, script: Option<&str>) {
        self.stage_counts.insert(node_id.to_string(), (0, 0));
        let display_name = match script {
            Some(s) => {
                let dim = Style::new().dim();
                format!("{name} {}", dim.apply_to(truncate(s, 60)))
            }
            None => name.to_string(),
        };
        if let ProgressRenderer::Tty(tty) = &self.renderer {
            if !self.any_stage_started {
                self.any_stage_started = true;
                let sep = tty.multi.add(ProgressBar::new_spinner());
                sep.set_style(style_empty());
                sep.finish();
            }
            let bar = tty.multi.add(ProgressBar::new_spinner());
            bar.set_style(style_stage_running());
            bar.set_message(display_name.clone());
            bar.enable_steady_tick(Duration::from_millis(100));
            self.active_stages.insert(
                node_id.to_string(),
                ActiveStage {
                    display_name,
                    has_model: false,
                    spinner: bar,
                    tool_calls: VecDeque::new(),
                    compaction_bar: None,
                },
            );
        }
    }

    fn finish_stage(&mut self, node_id: &str, name: &str, glyph: &str, prefix: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(_) => {
                if let Some(stage) = self.active_stages.remove(node_id) {
                    if let Some(bar) = stage.compaction_bar {
                        bar.finish_and_clear();
                    }
                    for entry in &stage.tool_calls {
                        if entry.is_branch || self.verbose {
                            // Keep visible: branches always, all entries in verbose mode
                            entry.bar.abandon();
                        } else {
                            entry.bar.finish_and_clear();
                        }
                    }
                    stage.spinner.set_style(style_stage_done());
                    stage.spinner.set_prefix(prefix.to_string());
                    stage
                        .spinner
                        .finish_with_message(format!("{glyph} {}", stage.display_name));
                }
            }
            ProgressRenderer::Plain => {
                if prefix.is_empty() {
                    eprintln!("    {glyph} {name}");
                } else {
                    eprintln!("    {glyph} {name}  {prefix}");
                }
            }
        }
    }

    // ── Agent / tool calls ──────────────────────────────────────────────

    fn on_agent_event(&mut self, stage_node_id: &str, event: &AgentEvent) {
        match event {
            AgentEvent::AssistantMessage { model, .. } => {
                if let Some(counts) = self.stage_counts.get_mut(stage_node_id) {
                    counts.0 += 1;
                }
                if let ProgressRenderer::Tty(_) = &self.renderer {
                    if let Some(stage) = self.active_stages.get_mut(stage_node_id) {
                        if !stage.has_model {
                            stage.has_model = true;
                            let dim = Style::new().dim();
                            let suffix = format!(" {}", dim.apply_to(format!("[{model}]")));
                            stage.display_name.push_str(&suffix);
                            stage.spinner.set_message(stage.display_name.clone());
                        }
                    }
                }
            }
            AgentEvent::ToolCallStarted {
                tool_name,
                tool_call_id,
                arguments,
            } => {
                self.on_tool_call_started(stage_node_id, tool_name, tool_call_id, arguments);
            }
            AgentEvent::ToolCallCompleted {
                tool_call_id,
                is_error,
                ..
            } => {
                if let Some(counts) = self.stage_counts.get_mut(stage_node_id) {
                    counts.1 += 1;
                }
                self.on_tool_call_completed(stage_node_id, tool_call_id, *is_error);
            }
            AgentEvent::Warning { kind, details, .. }
                if kind == "context_window" && self.verbose =>
            {
                let usage_percent = details
                    .get("usage_percent")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let yellow = Style::new().yellow();
                self.insert_info_line_for_stage(
                    stage_node_id,
                    &format!(
                        "{} context window: {usage_percent}% used",
                        yellow.apply_to("\u{26a0}")
                    ),
                );
            }
            AgentEvent::CompactionStarted { .. } => match &self.renderer {
                ProgressRenderer::Tty(tty) => {
                    if let Some(stage) = self.active_stages.get_mut(stage_node_id) {
                        if let Some(old) = stage.compaction_bar.take() {
                            old.finish_and_clear();
                        }
                        let bar = tty
                            .multi
                            .insert_after(stage.last_bar(), ProgressBar::new_spinner());
                        bar.set_style(style_tool_running());
                        bar.set_message("\u{27f3} compacting context\u{2026}");
                        bar.enable_steady_tick(Duration::from_millis(100));
                        stage.compaction_bar = Some(bar);
                    }
                }
                ProgressRenderer::Plain => {}
            },
            AgentEvent::CompactionCompleted {
                original_turn_count,
                preserved_turn_count,
                tracked_file_count,
                ..
            } => {
                let msg = format!(
                    "\u{27f3} compaction: {original_turn_count} \u{2192} {preserved_turn_count} turns, {tracked_file_count} files"
                );
                match &self.renderer {
                    ProgressRenderer::Tty(_) => {
                        if let Some(bar) = self
                            .active_stages
                            .get_mut(stage_node_id)
                            .and_then(|s| s.compaction_bar.take())
                        {
                            bar.set_style(style_tool_done());
                            bar.finish_with_message(msg);
                        } else {
                            self.insert_info_line_for_stage(stage_node_id, &msg);
                        }
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("      {msg}");
                    }
                }
            }
            AgentEvent::LlmRetry {
                model,
                attempt,
                delay_secs,
                error,
                ..
            } if self.verbose => {
                let yellow = Style::new().yellow();
                let delay_ms = (*delay_secs * 1000.0) as u64;
                let dur = format_duration_ms(delay_ms);
                self.insert_info_line_for_stage(
                    stage_node_id,
                    &format!(
                        "{} retry: {model} attempt {attempt} ({error}, delay {dur})",
                        yellow.apply_to("\u{26a0}")
                    ),
                );
            }
            AgentEvent::SubAgentSpawned { agent_id, task, .. } if self.verbose => {
                let dim = Style::new().dim();
                self.insert_subagent_line_for_stage(
                    stage_node_id,
                    &dim.apply_to(format!(
                        "\u{25b8} subagent[{agent_id}] \"{}\"",
                        truncate(task, 50)
                    ))
                    .to_string(),
                );
            }
            AgentEvent::SubAgentCompleted {
                agent_id,
                turns_used,
                success,
                ..
            } if self.verbose => {
                let glyph = if *success { green_check() } else { red_cross() };
                self.insert_subagent_line_for_stage(
                    stage_node_id,
                    &format!("{glyph} subagent[{agent_id}] ({turns_used} turns)"),
                );
            }
            _ => {}
        }
    }

    fn on_tool_call_started(
        &mut self,
        stage_node_id: &str,
        tool_name: &str,
        tool_call_id: &str,
        arguments: &serde_json::Value,
    ) {
        let display_name = self.tool_display_name(tool_name, arguments);

        if let ProgressRenderer::Tty(tty) = &self.renderer {
            if let Some(stage) = self.active_stages.get_mut(stage_node_id) {
                // Evict oldest if at capacity (prefer completed entries); skip in verbose mode
                if !self.verbose && stage.tool_calls.len() >= MAX_TOOL_CALLS {
                    let evict_idx = stage
                        .tool_calls
                        .iter()
                        .position(|e| !matches!(e.status, ToolCallStatus::Running))
                        .unwrap_or(0);
                    if let Some(evicted) = stage.tool_calls.remove(evict_idx) {
                        evicted.bar.finish_and_clear();
                    }
                }
                let bar = tty
                    .multi
                    .insert_after(stage.last_bar(), ProgressBar::new_spinner());
                bar.set_style(style_tool_running());
                bar.set_message(display_name.clone());
                bar.enable_steady_tick(Duration::from_millis(100));
                stage.tool_calls.push_back(ToolCallEntry {
                    display_name,
                    tool_call_id: tool_call_id.to_string(),
                    status: ToolCallStatus::Running,
                    bar,
                    is_branch: false,
                });
            }
        }
    }

    // ── Parallel branches ─────────────────────────────────────────────

    fn on_parallel_branch_started(&mut self, branch: &str) {
        let parent_id = match &self.parallel_parent {
            Some(id) => id.clone(),
            None => return,
        };

        if let ProgressRenderer::Tty(tty) = &self.renderer {
            if let Some(stage) = self.active_stages.get_mut(&parent_id) {
                let bar = tty
                    .multi
                    .insert_after(stage.last_bar(), ProgressBar::new_spinner());
                bar.set_style(style_subagent_info());
                let dim = Style::new().dim();
                bar.set_message(dim.apply_to(format!("\u{25b8} {branch}")).to_string());
                stage.tool_calls.push_back(ToolCallEntry {
                    display_name: branch.to_string(),
                    tool_call_id: branch.to_string(),
                    status: ToolCallStatus::Running,
                    bar,
                    is_branch: true,
                });
            }
        }
    }

    fn on_parallel_branch_completed(&mut self, branch: &str, duration_ms: u64, status: &str) {
        let succeeded = matches!(status, "success" | "partial_success");
        let glyph = if succeeded {
            green_check()
        } else {
            red_cross()
        };
        let dur = format_duration_ms(duration_ms);

        let parent_id = match &self.parallel_parent {
            Some(id) => id.clone(),
            None => return,
        };

        match &self.renderer {
            ProgressRenderer::Tty(_) => {
                if let Some(stage) = self.active_stages.get_mut(&parent_id) {
                    if let Some(entry) = stage
                        .tool_calls
                        .iter_mut()
                        .find(|e| e.tool_call_id == branch)
                    {
                        entry.status = if succeeded {
                            ToolCallStatus::Succeeded
                        } else {
                            ToolCallStatus::Failed
                        };
                        let elapsed = format_duration_short(entry.bar.elapsed());
                        entry.bar.set_style(style_branch_done());
                        entry.bar.set_prefix(elapsed);
                        entry
                            .bar
                            .finish_with_message(format!("{glyph} {}", entry.display_name));
                    }
                }
            }
            ProgressRenderer::Plain => {
                eprintln!("        {glyph} {branch}  {dur}");
            }
        }
    }

    /// Insert a static info line (verbose-only) at the current position.
    fn insert_info_line(&mut self, message: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(message.to_string());
            }
            ProgressRenderer::Plain => {
                eprintln!("    {message}");
            }
        }
    }

    /// Insert a static info line nested under a stage's tool calls.
    fn insert_info_line_for_stage(&mut self, stage_node_id: &str, message: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = if let Some(stage) = self.active_stages.get(stage_node_id) {
                    tty.multi
                        .insert_after(stage.last_bar(), ProgressBar::new_spinner())
                } else {
                    tty.multi.add(ProgressBar::new_spinner())
                };
                bar.set_style(style_tool_done());
                bar.finish_with_message(message.to_string());
            }
            ProgressRenderer::Plain => {
                eprintln!("      {message}");
            }
        }
    }

    fn on_run_notice(&mut self, level: RunNoticeLevel, code: &str, message: &str) {
        let dim = Style::new().dim();
        let label = match level {
            RunNoticeLevel::Info => Style::new().bold().apply_to("Info:"),
            RunNoticeLevel::Warn => Style::new().yellow().apply_to("Warning:"),
            RunNoticeLevel::Error => Style::new().red().apply_to("Error:"),
        };
        let code_suffix = if code.is_empty() {
            String::new()
        } else {
            format!(" {}", dim.apply_to(format!("[{code}]")))
        };
        self.insert_info_line(&format!("{label} {message}{code_suffix}"));
    }

    fn on_pull_request_created(&mut self, pr_url: &str, draft: bool) {
        let label = if draft { "Draft PR:" } else { "PR:" };
        let bold = Style::new().bold();
        self.insert_info_line(&format!("{} {pr_url}", bold.apply_to(label)));
    }

    fn on_pull_request_failed(&mut self, error: &str) {
        let red = Style::new().red();
        self.insert_info_line(&format!("{} {error}", red.apply_to("PR failed:")));
    }

    /// Insert a static info line for a subagent, indented deeper than tool calls.
    fn insert_subagent_line_for_stage(&mut self, stage_node_id: &str, message: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = if let Some(stage) = self.active_stages.get(stage_node_id) {
                    tty.multi
                        .insert_after(stage.last_bar(), ProgressBar::new_spinner())
                } else {
                    tty.multi.add(ProgressBar::new_spinner())
                };
                bar.set_style(style_subagent_info());
                bar.finish_with_message(message.to_string());
            }
            ProgressRenderer::Plain => {
                eprintln!("        {message}");
            }
        }
    }

    fn on_tool_call_completed(&mut self, stage_node_id: &str, tool_call_id: &str, is_error: bool) {
        if let ProgressRenderer::Tty(_) = &self.renderer {
            if let Some(stage) = self.active_stages.get_mut(stage_node_id) {
                if let Some(entry) = stage
                    .tool_calls
                    .iter_mut()
                    .find(|e| e.tool_call_id == tool_call_id)
                {
                    let glyph = if is_error { red_cross() } else { green_check() };
                    entry.status = if is_error {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Succeeded
                    };
                    let elapsed = format_duration_short(entry.bar.elapsed());
                    entry.bar.set_style(style_tool_done());
                    entry.bar.set_prefix(elapsed);
                    entry
                        .bar
                        .finish_with_message(format!("{glyph} {}", entry.display_name));
                }
            }
        }
    }
}

// ── ProgressAwareInterviewer ────────────────────────────────────────────

/// Wraps a `ConsoleInterviewer` so that progress bars are hidden during
/// interactive prompts (avoids garbled output from concurrent writes).
pub struct ProgressAwareInterviewer {
    inner: ConsoleInterviewer,
    progress: Arc<Mutex<ProgressUI>>,
}

impl ProgressAwareInterviewer {
    pub fn new(inner: ConsoleInterviewer, progress: Arc<Mutex<ProgressUI>>) -> Self {
        Self { inner, progress }
    }
}

#[async_trait]
impl Interviewer for ProgressAwareInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        self.progress
            .lock()
            .expect("progress lock poisoned")
            .hide_bars();
        let answer = self.inner.ask(question).await;
        self.progress
            .lock()
            .expect("progress lock poisoned")
            .show_bars();
        answer
    }

    async fn inform(&self, message: &str, stage: &str) {
        self.progress
            .lock()
            .expect("progress lock poisoned")
            .hide_bars();
        self.inner.inform(message, stage).await;
        self.progress
            .lock()
            .expect("progress lock poisoned")
            .show_bars();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage_started(node_id: &str, name: &str) -> WorkflowRunEvent {
        WorkflowRunEvent::StageStarted {
            node_id: node_id.into(),
            name: name.into(),
            index: 0,
            handler_type: None,
            script: None,
            attempt: 1,
            max_attempts: 1,
        }
    }

    #[test]
    fn parallel_branches_tracked_as_tool_calls() {
        let mut ui = ProgressUI::new(true, false);

        ui.handle_event(&stage_started("fork1", "Fork Analysis"));
        assert!(ui.active_stages.contains_key("fork1"));
        assert!(ui.parallel_parent.is_none());

        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 2,
            join_policy: "wait_all".into(),
        });
        assert_eq!(ui.parallel_parent.as_deref(), Some("fork1"));

        // Branch started → creates a tool_call entry
        ui.handle_event(&WorkflowRunEvent::ParallelBranchStarted {
            branch: "security".into(),
            index: 0,
        });
        let stage = ui.active_stages.get("fork1").unwrap();
        assert_eq!(stage.tool_calls.len(), 1);
        assert_eq!(stage.tool_calls[0].tool_call_id, "security");
        assert!(matches!(
            stage.tool_calls[0].status,
            ToolCallStatus::Running
        ));

        // Branch completed → marks entry as succeeded
        ui.handle_event(&WorkflowRunEvent::ParallelBranchCompleted {
            branch: "security".into(),
            index: 0,
            duration_ms: 2000,
            status: "success".into(),
        });
        let stage = ui.active_stages.get("fork1").unwrap();
        assert!(matches!(
            stage.tool_calls[0].status,
            ToolCallStatus::Succeeded
        ));

        // Second branch
        ui.handle_event(&WorkflowRunEvent::ParallelBranchStarted {
            branch: "quality".into(),
            index: 1,
        });
        ui.handle_event(&WorkflowRunEvent::ParallelBranchCompleted {
            branch: "quality".into(),
            index: 1,
            duration_ms: 3000,
            status: "success".into(),
        });
        let stage = ui.active_stages.get("fork1").unwrap();
        assert_eq!(stage.tool_calls.len(), 2);

        // Parallel completed → clears parent
        ui.handle_event(&WorkflowRunEvent::ParallelCompleted {
            duration_ms: 3000,
            success_count: 2,
            failure_count: 0,
        });
        assert!(ui.parallel_parent.is_none());
    }

    #[test]
    fn parallel_branch_running_shows_triangle_glyph() {
        let mut ui = ProgressUI::new(true, false);

        ui.handle_event(&stage_started("fork1", "Fork"));
        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 1,
            join_policy: "wait_all".into(),
        });
        ui.handle_event(&WorkflowRunEvent::ParallelBranchStarted {
            branch: "security".into(),
            index: 0,
        });

        let stage = ui.active_stages.get("fork1").unwrap();
        let bar = &stage.tool_calls[0].bar;
        let msg = bar.message();
        assert!(
            msg.contains('\u{25b8}'),
            "expected bar message to contain ▸, got: {msg:?}"
        );
    }

    #[test]
    fn parallel_branch_failure_tracked() {
        let mut ui = ProgressUI::new(true, false);

        ui.handle_event(&stage_started("fork1", "Fork"));
        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 1,
            join_policy: "wait_all".into(),
        });
        ui.handle_event(&WorkflowRunEvent::ParallelBranchStarted {
            branch: "risky".into(),
            index: 0,
        });
        ui.handle_event(&WorkflowRunEvent::ParallelBranchCompleted {
            branch: "risky".into(),
            index: 0,
            duration_ms: 500,
            status: "fail".into(),
        });

        let stage = ui.active_stages.get("fork1").unwrap();
        assert!(matches!(stage.tool_calls[0].status, ToolCallStatus::Failed));
    }

    #[test]
    fn compaction_sets_and_clears_bar() {
        let mut ui = ProgressUI::new(true, false);

        ui.handle_event(&stage_started("s1", "Build"));
        assert!(ui.active_stages["s1"].compaction_bar.is_none());

        ui.handle_event(&WorkflowRunEvent::Agent {
            stage: "s1".into(),
            event: AgentEvent::CompactionStarted {
                estimated_tokens: 5000,
                context_window_size: 8000,
            },
        });
        assert!(ui.active_stages["s1"].compaction_bar.is_some());

        ui.handle_event(&WorkflowRunEvent::Agent {
            stage: "s1".into(),
            event: AgentEvent::CompactionCompleted {
                original_turn_count: 20,
                preserved_turn_count: 6,
                summary_token_estimate: 500,
                tracked_file_count: 3,
            },
        });
        assert!(ui.active_stages["s1"].compaction_bar.is_none());
    }

    #[test]
    fn tool_display_name_shortens_path_relative_to_working_directory() {
        let mut ui = ProgressUI::new(true, false);
        ui.set_working_directory("/home/daytona/workspace".to_string());

        let args = serde_json::json!({"file_path": "/home/daytona/workspace/output/js/physics.js"});
        let display = ui.tool_display_name("write_file", &args);
        assert!(
            display.contains("output/js/physics.js"),
            "expected relative path in: {display}"
        );
        assert!(
            !display.contains("/home/daytona/workspace/"),
            "should not contain absolute working dir in: {display}"
        );
    }

    #[test]
    fn tool_display_name_preserves_path_outside_working_directory() {
        let mut ui = ProgressUI::new(true, false);
        ui.set_working_directory("/home/daytona/workspace".to_string());

        let args = serde_json::json!({"file_path": "/etc/config.json"});
        let display = ui.tool_display_name("read_file", &args);
        assert!(
            display.contains("/etc/config.json"),
            "expected absolute path preserved in: {display}"
        );
    }

    #[test]
    fn tool_display_name_without_working_directory_shows_full_path() {
        let ui = ProgressUI::new(true, false);

        let args = serde_json::json!({"file_path": "/home/daytona/workspace/output/js/physics.js"});
        let display = ui.tool_display_name("write_file", &args);
        assert!(
            display.contains("/home/daytona/workspace/output/js/physics.js"),
            "expected full path when no working dir set: {display}"
        );
    }

    #[test]
    fn plain_mode_sets_parallel_parent() {
        let mut ui = ProgressUI::new(false, false);

        ui.handle_event(&stage_started("fork1", "Fork"));
        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 2,
            join_policy: "wait_all".into(),
        });
        // In Plain mode, active_stages is empty so parallel_parent is a sentinel
        assert!(ui.parallel_parent.is_some());

        ui.handle_event(&WorkflowRunEvent::ParallelCompleted {
            duration_ms: 1000,
            success_count: 2,
            failure_count: 0,
        });
        assert!(ui.parallel_parent.is_none());
    }

    #[test]
    fn handle_json_line_stage_started_and_completed() {
        let mut ui = ProgressUI::new(false, false);

        let started = r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_id":"plan","node_label":"Plan","stage_index":0,"script":null,"attempt":1,"max_attempts":1}"#;
        ui.handle_json_line(started);
        assert!(ui.stage_counts.contains_key("plan"));

        let completed = r#"{"ts":"2026-01-01T12:00:10Z","event":"StageCompleted","node_id":"plan","node_label":"Plan","stage_index":0,"duration_ms":10000,"status":"success"}"#;
        ui.handle_json_line(completed);
        // In Plain mode, finish_stage just prints, so verify no panic
    }

    #[test]
    fn handle_json_line_tool_call_round_trip() {
        let mut ui = ProgressUI::new(false, true); // verbose

        // Start a stage first
        let started = r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_id":"code","node_label":"Code","stage_index":0,"attempt":1,"max_attempts":1}"#;
        ui.handle_json_line(started);

        let tc_start = r#"{"ts":"2026-01-01T12:00:01Z","event":"Agent.ToolCallStarted","node_id":"code","node_label":"code","tool_name":"read_file","tool_call_id":"tc1","arguments":{"path":"src/main.rs"}}"#;
        ui.handle_json_line(tc_start);
        assert_eq!(ui.stage_counts.get("code").map(|c| c.1), Some(1));

        let tc_done = r#"{"ts":"2026-01-01T12:00:02Z","event":"Agent.ToolCallCompleted","node_id":"code","node_label":"code","tool_name":"read_file","tool_call_id":"tc1","is_error":false}"#;
        ui.handle_json_line(tc_done);
    }

    #[test]
    fn handle_json_line_retro_events() {
        let mut ui = ProgressUI::new(false, false);

        let retro_started = r#"{"ts":"2026-01-01T12:00:00Z","event":"RetroStarted"}"#;
        ui.handle_json_line(retro_started);

        let retro_completed =
            r#"{"ts":"2026-01-01T12:00:05Z","event":"RetroCompleted","duration_ms":5000}"#;
        ui.handle_json_line(retro_completed);
    }

    #[test]
    fn handle_json_line_ignores_invalid_json() {
        let mut ui = ProgressUI::new(false, false);
        ui.handle_json_line("not valid json");
        ui.handle_json_line("");
        ui.handle_json_line("{}"); // no event field
    }

    // ── Bug regression tests (post-rename JSONL field names) ─────────

    // Bug 1: handle_json_line reads pre-rename field names but real JSONL
    // uses post-rename names from rename_fields(). These tests use the
    // actual JSONL format produced by the engine.

    #[test]
    fn bug1_stage_started_uses_node_label_not_name() {
        // Real JSONL: rename_fields renames "name" → "node_label"
        let mut ui = ProgressUI::new(true, false);
        let started = r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_id":"plan","node_label":"Plan","stage_index":0,"script":null,"attempt":1,"max_attempts":1}"#;
        ui.handle_json_line(started);

        let stage = ui
            .active_stages
            .get("plan")
            .expect("stage should be tracked");
        assert_eq!(
            stage.display_name, "Plan",
            "display name should come from node_label field, not be '?'"
        );
    }

    #[test]
    fn bug1_agent_tool_call_uses_node_id_not_stage() {
        // Real JSONL: rename_fields renames "stage" → "node_id" for Agent.* events
        let mut ui = ProgressUI::new(false, true);

        // First create the stage
        let started = r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_id":"code","node_label":"Code","stage_index":0,"attempt":1,"max_attempts":1}"#;
        ui.handle_json_line(started);
        assert_eq!(ui.stage_counts.get("code").map(|c| c.1), Some(0));

        // Tool call with post-rename field: "node_id" instead of "stage"
        let tc_start = r#"{"ts":"2026-01-01T12:00:01Z","event":"Agent.ToolCallStarted","node_id":"code","node_label":"code","tool_name":"read_file","tool_call_id":"tc1","arguments":{"path":"src/main.rs"}}"#;
        ui.handle_json_line(tc_start);

        assert_eq!(
            ui.stage_counts.get("code").map(|c| c.1),
            Some(1),
            "tool call count should increment using node_id field"
        );
    }

    #[test]
    fn bug1_agent_assistant_message_uses_node_id_not_stage() {
        // Real JSONL: rename_fields renames "stage" → "node_id"
        let mut ui = ProgressUI::new(false, true);

        let started = r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_id":"code","node_label":"Code","stage_index":0,"attempt":1,"max_attempts":1}"#;
        ui.handle_json_line(started);
        assert_eq!(ui.stage_counts.get("code").map(|c| c.0), Some(0));

        // AssistantMessage with post-rename field: "node_id" instead of "stage"
        let msg = r#"{"ts":"2026-01-01T12:00:01Z","event":"Agent.AssistantMessage","node_id":"code","node_label":"code","model":"claude-sonnet-4-20250514"}"#;
        ui.handle_json_line(msg);

        assert_eq!(
            ui.stage_counts.get("code").map(|c| c.0),
            Some(1),
            "turn count should increment using node_id field"
        );
    }

    #[test]
    fn bug1_parallel_branch_uses_node_id_not_branch() {
        // Real JSONL: rename_fields renames "branch" → "node_id"
        let mut ui = ProgressUI::new(true, false);

        // Set up a parent stage and start parallel
        let parent = r#"{"ts":"2026-01-01T12:00:00Z","event":"StageStarted","node_id":"fork","node_label":"Fork","stage_index":0,"attempt":1,"max_attempts":1}"#;
        ui.handle_json_line(parent);
        let par = r#"{"ts":"2026-01-01T12:00:01Z","event":"ParallelStarted","branch_count":2,"join_policy":"wait_all"}"#;
        ui.handle_json_line(par);
        assert!(ui.parallel_parent.is_some());

        // ParallelBranchStarted with post-rename field: "node_id" instead of "branch"
        let branch = r#"{"ts":"2026-01-01T12:00:02Z","event":"ParallelBranchStarted","node_id":"lint","node_label":"lint","branch_index":0}"#;
        ui.handle_json_line(branch);

        // Branch should have been registered as a tool_call entry on the parent
        let parent_stage = ui.active_stages.get("fork").unwrap();
        assert!(
            !parent_stage.tool_calls.is_empty(),
            "parallel branch should be registered using node_id field"
        );
    }

    // Bug 5: start_run should write Starting status before spawning engine
    // (tested in start.rs)

    // Bug 6: handle_json_line is missing devcontainer event dispatch

    #[test]
    fn bug6_devcontainer_lifecycle_started_dispatched() {
        let mut ui = ProgressUI::new(false, false);
        let event = r#"{"ts":"2026-01-01T12:00:00Z","event":"DevcontainerLifecycleStarted","phase":"postCreate","command_count":2}"#;
        ui.handle_json_line(event);
        assert_eq!(
            ui.devcontainer_command_count, 2,
            "devcontainer_command_count should be set by DevcontainerLifecycleStarted"
        );
    }

    #[test]
    fn handle_json_line_run_notice_warn() {
        let mut ui = ProgressUI::new(false, false);
        let event = r#"{"ts":"2026-01-01T12:00:00Z","event":"RunNotice","level":"warn","code":"sandbox_cleanup_failed","message":"sandbox cleanup failed: boom"}"#;
        ui.handle_json_line(event);
    }

    #[test]
    fn handle_json_line_pull_request_failed() {
        let mut ui = ProgressUI::new(false, false);
        let event = r#"{"ts":"2026-01-01T12:00:00Z","event":"PullRequestFailed","error":"auth token expired"}"#;
        ui.handle_json_line(event);
    }
}
