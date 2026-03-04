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
    "          {spinner:.dim} {wide_msg} {elapsed:.dim}"
);
cached_style!(style_tool_done, "          {wide_msg}");
cached_style!(style_static_dim, "    {wide_msg:.dim}");
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

fn format_duration_short(d: Duration) -> String {
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

// ── Tool call display name ──────────────────────────────────────────────

fn tool_display_name(tool_name: &str, arguments: &serde_json::Value) -> String {
    if tool_name == "bash" || tool_name == "execute_command" {
        if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
            let truncated: String = if cmd.len() > 60 {
                let mut s: String = cmd.chars().take(57).collect();
                s.push_str("...");
                s
            } else {
                cmd.to_string()
            };
            return format!("bash: {truncated}");
        }
    }
    tool_name.to_string()
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
}

// ── Active stage ────────────────────────────────────────────────────────

struct ActiveStage {
    name: String,
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
                entry.bar.finish_and_clear();
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
            WorkflowRunEvent::StageStarted { node_id, name, .. } => {
                self.on_stage_started(node_id, name);
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
            } => {
                let dur = format_duration_ms(*duration_ms);
                match &self.renderer {
                    ProgressRenderer::Tty(_) => {
                        if let Some(bar) = self.sandbox_bar.take() {
                            bar.set_style(style_header_done());
                            bar.set_prefix(dur);
                            bar.finish_with_message(format!("Sandbox: {provider}"));
                        }
                    }
                    ProgressRenderer::Plain => {
                        eprintln!("    Sandbox: {provider} (ready in {dur})");
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
        let path_str = logs_dir.display().to_string();
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

    fn on_stage_started(&mut self, node_id: &str, name: &str) {
        if let ProgressRenderer::Tty(tty) = &self.renderer {
            if !self.any_stage_started {
                self.any_stage_started = true;
                let sep = tty.multi.add(ProgressBar::new_spinner());
                sep.set_style(style_empty());
                sep.finish();
            }
            let bar = tty.multi.add(ProgressBar::new_spinner());
            bar.set_style(style_stage_running());
            bar.set_message(name.to_string());
            bar.enable_steady_tick(Duration::from_millis(100));
            self.active_stages.insert(
                node_id.to_string(),
                ActiveStage {
                    name: name.to_string(),
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
                        entry.bar.finish_and_clear();
                    }
                    stage.spinner.set_style(style_stage_done());
                    stage.spinner.set_prefix(prefix.to_string());
                    stage
                        .spinner
                        .finish_with_message(format!("{glyph} {}", stage.name));
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
                });
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
                    entry.bar.set_style(style_tool_done());
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
