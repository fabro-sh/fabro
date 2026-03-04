use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use console::Style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::event::{EventEmitter, WorkflowRunEvent};
use crate::interviewer::{Answer, Interviewer, Question};
use crate::outcome::StageStatus;
use arc_agent::AgentEvent;

use super::{compute_stage_cost, format_cost};

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

fn format_duration_ms(ms: u64) -> String {
    format_duration_short(Duration::from_millis(ms))
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
    if s.len() > max {
        let mut t: String = s.chars().take(max - 3).collect();
        t.push_str("...");
        t
    } else {
        s.to_string()
    }
}

fn shorten_path(path: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = std::path::Path::new(path).strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    path.to_string()
}

fn tool_display_name(tool_name: &str, arguments: &serde_json::Value) -> String {
    let dim = Style::new().dim();
    let arg = |key: &str| arguments.get(key).and_then(|v| v.as_str());
    let path_arg = || arg("path").or_else(|| arg("file_path")).map(|p| truncate(&shorten_path(p), 60));

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
        "use_skill" => arg("skill_name").map(String::from),
        _ => None,
    };

    match detail {
        Some(d) => format!("{tool_name}{}", dim.apply_to(format!("({d})"))),
        None => tool_name.to_string(),
    }
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
    active_stages: HashMap<String, ActiveStage>,
    setup_command_count: usize,
    sandbox_bar: Option<ProgressBar>,
    setup_bar: Option<ProgressBar>,
    any_stage_started: bool,
    parallel_parent: Option<String>,
}

impl ProgressUI {
    pub fn new(is_tty: bool) -> Self {
        let renderer = if is_tty {
            ProgressRenderer::Tty(TtyRenderer {
                multi: MultiProgress::new(),
            })
        } else {
            ProgressRenderer::Plain
        };
        Self {
            renderer,
            active_stages: HashMap::new(),
            setup_command_count: 0,
            sandbox_bar: None,
            setup_bar: None,
            any_stage_started: false,
            parallel_parent: None,
        }
    }

    /// Register event handlers on the emitter.
    pub fn register(progress: &Arc<Mutex<Self>>, emitter: &mut EventEmitter) {
        let p = Arc::clone(progress);
        emitter.on_event(move |event| {
            let mut ui = p.lock().expect("progress lock poisoned");
            ui.handle_event(event);
        });
    }

    /// Clear all active bars and release the terminal for normal stderr output.
    pub fn finish(&mut self) {
        for (_id, stage) in self.active_stages.drain() {
            for entry in &stage.tool_calls {
                if entry.is_branch {
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

    fn handle_event(&mut self, event: &WorkflowRunEvent) {
        match event {
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
                let prefix = format!("{cost_str}{dur}");
                let glyph = if succeeded {
                    green_check()
                } else {
                    red_cross()
                };
                self.finish_stage(node_id, name, glyph, &prefix);
            }
            WorkflowRunEvent::StageFailed { node_id, name, .. } => {
                self.finish_stage(node_id, name, red_cross(), "");
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
            _ => {}
        }
    }

    // ── Sandbox ─────────────────────────────────────────────────────────

    fn on_sandbox_event(&mut self, event: &arc_agent::SandboxEvent) {
        use arc_agent::SandboxEvent;
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
                    (Some(n), Some(c), Some(m)) => {
                        Some(format!("{n} ({} cpu, {} GB)", format_number(*c), format_number(*m)))
                    }
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
                                let detail_bar = tty.multi.insert_after(&bar, ProgressBar::new_spinner());
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

    // ── Logs dir (called externally) ────────────────────────────────────

    pub fn show_logs_dir(&mut self, logs_dir: &Path) {
        let path_str = super::tilde_path(logs_dir);
        match &self.renderer {
            ProgressRenderer::Tty(tty) => {
                let bar = tty.multi.add(ProgressBar::new_spinner());
                bar.set_style(style_static_dim());
                bar.finish_with_message(format!("Logs: {path_str}"));
            }
            ProgressRenderer::Plain => {
                eprintln!("    Logs: {path_str}");
            }
        }
    }

    // ── Stages ──────────────────────────────────────────────────────────

    fn on_stage_started(&mut self, node_id: &str, name: &str, script: Option<&str>) {
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
                },
            );
        }
    }

    fn finish_stage(&mut self, node_id: &str, name: &str, glyph: &str, prefix: &str) {
        match &self.renderer {
            ProgressRenderer::Tty(_) => {
                if let Some(stage) = self.active_stages.remove(node_id) {
                    for entry in &stage.tool_calls {
                        if entry.is_branch {
                            // Already finished by on_parallel_branch_completed; keep visible
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
                self.on_tool_call_completed(stage_node_id, tool_call_id, *is_error);
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
        let display_name = tool_display_name(tool_name, arguments);

        if let ProgressRenderer::Tty(tty) = &self.renderer {
            if let Some(stage) = self.active_stages.get_mut(stage_node_id) {
                // Evict oldest if at capacity (prefer completed entries)
                if stage.tool_calls.len() >= MAX_TOOL_CALLS {
                    let evict_idx = stage
                        .tool_calls
                        .iter()
                        .position(|e| !matches!(e.status, ToolCallStatus::Running))
                        .unwrap_or(0);
                    if let Some(evicted) = stage.tool_calls.remove(evict_idx) {
                        evicted.bar.finish_and_clear();
                    }
                }
                let bar = tty.multi.insert_after(
                    stage.tool_calls.back().map_or(&stage.spinner, |e| &e.bar),
                    ProgressBar::new_spinner(),
                );
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
                let bar = tty.multi.insert_after(
                    stage.tool_calls.back().map_or(&stage.spinner, |e| &e.bar),
                    ProgressBar::new_spinner(),
                );
                bar.set_style(style_tool_running());
                bar.set_message(branch.to_string());
                bar.enable_steady_tick(Duration::from_millis(100));
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
        let glyph = if succeeded { green_check() } else { red_cross() };
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
                        entry.bar.set_style(style_tool_done());
                        entry.bar.set_prefix(elapsed);
                        entry
                            .bar
                            .finish_with_message(format!("{glyph} {}", entry.display_name));
                    }
                }
            }
            ProgressRenderer::Plain => {
                eprintln!("      {glyph} {branch}  {dur}");
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
    inner: crate::interviewer::console::ConsoleInterviewer,
    progress: Arc<Mutex<ProgressUI>>,
}

impl ProgressAwareInterviewer {
    pub fn new(
        inner: crate::interviewer::console::ConsoleInterviewer,
        progress: Arc<Mutex<ProgressUI>>,
    ) -> Self {
        Self { inner, progress }
    }

    fn hide_bars(&self) {
        let ui = self.progress.lock().expect("progress lock poisoned");
        if let ProgressRenderer::Tty(tty) = &ui.renderer {
            tty.multi.set_draw_target(ProgressDrawTarget::hidden());
        }
    }

    fn show_bars(&self) {
        let ui = self.progress.lock().expect("progress lock poisoned");
        if let ProgressRenderer::Tty(tty) = &ui.renderer {
            tty.multi.set_draw_target(ProgressDrawTarget::stderr());
        }
    }
}

#[async_trait]
impl Interviewer for ProgressAwareInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        {
            let ui = self.progress.lock().expect("progress lock poisoned");
            if let ProgressRenderer::Tty(tty) = &ui.renderer {
                let sep = tty.multi.add(ProgressBar::new_spinner());
                sep.set_style(style_empty());
                sep.finish();
                tty.multi.set_draw_target(ProgressDrawTarget::hidden());
            }
        }
        let answer = self.inner.ask(question).await;
        self.show_bars();
        answer
    }

    async fn inform(&self, message: &str, stage: &str) {
        self.hide_bars();
        self.inner.inform(message, stage).await;
        self.show_bars();
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
        let mut ui = ProgressUI::new(true);

        ui.handle_event(&stage_started("fork1", "Fork Analysis"));
        assert!(ui.active_stages.contains_key("fork1"));
        assert!(ui.parallel_parent.is_none());

        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 2,
            join_policy: "wait_all".into(),
            error_policy: "continue".into(),
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
        assert!(matches!(stage.tool_calls[0].status, ToolCallStatus::Running));

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
    fn parallel_branch_failure_tracked() {
        let mut ui = ProgressUI::new(true);

        ui.handle_event(&stage_started("fork1", "Fork"));
        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 1,
            join_policy: "wait_all".into(),
            error_policy: "continue".into(),
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
        assert!(matches!(
            stage.tool_calls[0].status,
            ToolCallStatus::Failed
        ));
    }

    #[test]
    fn plain_mode_sets_parallel_parent() {
        let mut ui = ProgressUI::new(false);

        ui.handle_event(&stage_started("fork1", "Fork"));
        ui.handle_event(&WorkflowRunEvent::ParallelStarted {
            branch_count: 2,
            join_policy: "wait_all".into(),
            error_policy: "continue".into(),
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
}
