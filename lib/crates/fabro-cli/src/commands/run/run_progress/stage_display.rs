use std::collections::{HashMap, VecDeque};
use std::convert::TryFrom;
use std::time::Duration;

use chrono::{DateTime, Utc};
use fabro_workflow::outcome::{StageOutcome, format_cost};
use indicatif::ProgressBar;

use super::event::ProgressUsage;
use super::renderer::ProgressRenderer;
use super::styles;
use crate::shared::{format_duration_ms, format_tokens_human};

const MAX_TOOL_CALLS: usize = 5;

#[derive(Debug)]
pub(super) enum ToolCallStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug)]
pub(super) struct ToolCallEntry {
    pub(super) display_name: String,
    pub(super) tool_call_id: String,
    pub(super) status:       ToolCallStatus,
    pub(super) bar:          ProgressBar,
    pub(super) is_branch:    bool,
    pub(super) started_at:   Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub(super) struct ActiveStage {
    pub(super) display_name:   String,
    pub(super) has_model:      bool,
    pub(super) spinner:        ProgressBar,
    pub(super) tool_calls:     VecDeque<ToolCallEntry>,
    pub(super) compaction_bar: Option<ProgressBar>,
}

impl ActiveStage {
    fn last_bar(&self) -> &ProgressBar {
        self.tool_calls
            .back()
            .map_or(&self.spinner, |entry| &entry.bar)
    }
}

pub(super) struct StageDisplay {
    verbose:                    bool,
    pub(super) active_stages:   HashMap<String, ActiveStage>,
    pub(super) stage_counts:    HashMap<String, (u64, u64)>,
    pub(super) parallel_parent: Option<String>,
    any_stage_started:          bool,
    working_directory:          Option<String>,
}

impl StageDisplay {
    pub(super) fn new(verbose: bool) -> Self {
        Self {
            verbose,
            active_stages: HashMap::new(),
            stage_counts: HashMap::new(),
            parallel_parent: None,
            any_stage_started: false,
            working_directory: None,
        }
    }

    pub(super) fn set_working_directory(&mut self, dir: String) {
        self.working_directory = Some(dir);
    }

    pub(super) fn finish(&mut self) {
        for (_node_id, stage) in self.active_stages.drain() {
            if let Some(bar) = stage.compaction_bar {
                bar.finish_and_clear();
            }
            for entry in &stage.tool_calls {
                if entry.is_branch || self.verbose {
                    entry.bar.abandon();
                } else {
                    entry.bar.finish_and_clear();
                }
            }
            stage.spinner.finish_and_clear();
        }
    }

    pub(super) fn on_stage_started(
        &mut self,
        renderer: &ProgressRenderer,
        node_id: &str,
        name: &str,
        script: Option<&str>,
    ) {
        self.stage_counts.insert(node_id.to_string(), (0, 0));
        let display_name = match script {
            Some(script) => format!(
                "{name} {}",
                renderer.styles().dim.apply_to(styles::truncate(script, 60))
            ),
            None => name.to_string(),
        };

        if renderer.is_tty() && !self.any_stage_started {
            self.any_stage_started = true;
            let sep = renderer.add_spinner();
            sep.set_style(styles::style_empty());
            sep.finish();
        }

        let bar = renderer.add_spinner();
        bar.set_style(styles::style_stage_running());
        bar.set_message(display_name.clone());
        if renderer.is_tty() {
            bar.enable_steady_tick(Duration::from_millis(100));
        }
        self.active_stages.insert(node_id.to_string(), ActiveStage {
            display_name,
            has_model: false,
            spinner: bar,
            tool_calls: VecDeque::new(),
            compaction_bar: None,
        });
    }

    pub(super) fn on_stage_completed(
        &mut self,
        renderer: &ProgressRenderer,
        node_id: &str,
        name: &str,
        duration_ms: u64,
        status: &str,
        usage: Option<&ProgressUsage>,
    ) {
        let succeeded = status.parse::<StageOutcome>().map_or_else(
            |_| matches!(status, "succeeded" | "partially_succeeded"),
            |status| {
                matches!(
                    status,
                    StageOutcome::Succeeded | StageOutcome::PartiallySucceeded
                )
            },
        );
        let cost_str = usage
            .and_then(ProgressUsage::display_cost)
            .map(|cost| format!("{}   ", format_cost(cost)))
            .unwrap_or_default();
        let stats_str = if self.verbose {
            let (turn_count, tool_call_count) =
                self.stage_counts.get(node_id).copied().unwrap_or((0, 0));
            let total_tokens = usage.map_or(0, ProgressUsage::total_tokens);
            if turn_count > 0 || tool_call_count > 0 || total_tokens > 0 {
                let total_tokens = i64::try_from(total_tokens).unwrap_or(i64::MAX);
                format!(
                    "  {}",
                    renderer.styles().dim.apply_to(format!(
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
        let prefix = format!("{cost_str}{}{stats_str}", format_duration_ms(duration_ms));
        let glyph = if succeeded {
            styles::green_check(renderer.styles())
        } else {
            styles::red_cross(renderer.styles())
        };
        self.finish_stage(renderer, node_id, name, &glyph, &prefix);
    }

    pub(super) fn on_stage_failed(
        &mut self,
        renderer: &ProgressRenderer,
        node_id: &str,
        name: &str,
        error: &str,
    ) {
        self.finish_stage(
            renderer,
            node_id,
            name,
            &styles::red_cross(renderer.styles()),
            "",
        );
        let summary = styles::last_line_truncated(error, 120);
        Self::insert_global_info_line(
            renderer,
            &format!("{} {summary}", renderer.styles().red.apply_to("Error:")),
        );
    }

    pub(super) fn on_parallel_started(&mut self) {
        self.parallel_parent = self
            .active_stages
            .keys()
            .next()
            .cloned()
            .or_else(|| Some(String::new()));
    }

    pub(super) fn on_parallel_completed(&mut self) {
        self.parallel_parent = None;
    }

    pub(super) fn on_parallel_branch_started(&mut self, renderer: &ProgressRenderer, branch: &str) {
        let Some(parent_id) = self.parallel_parent.clone() else {
            return;
        };
        let Some(stage) = self.active_stages.get_mut(&parent_id) else {
            return;
        };

        let bar = renderer.insert_after(stage.last_bar());
        bar.set_style(styles::style_subagent_info());
        bar.set_message(
            renderer
                .styles()
                .dim
                .apply_to(format!("\u{25b8} {branch}"))
                .to_string(),
        );
        stage.tool_calls.push_back(ToolCallEntry {
            display_name: branch.to_string(),
            tool_call_id: branch.to_string(),
            status: ToolCallStatus::Running,
            bar,
            is_branch: true,
            started_at: None,
        });
    }

    pub(super) fn on_parallel_branch_completed(
        &mut self,
        renderer: &ProgressRenderer,
        branch: &str,
        duration_ms: u64,
        status: &str,
    ) {
        let Some(parent_id) = self.parallel_parent.clone() else {
            return;
        };
        let Some(stage) = self.active_stages.get_mut(&parent_id) else {
            return;
        };

        let Some(entry) = stage
            .tool_calls
            .iter_mut()
            .find(|entry| entry.tool_call_id == branch)
        else {
            return;
        };

        let succeeded = matches!(status, "succeeded" | "partially_succeeded");
        entry.status = if succeeded {
            ToolCallStatus::Succeeded
        } else {
            ToolCallStatus::Failed
        };
        let glyph = if succeeded {
            styles::green_check(renderer.styles())
        } else {
            styles::red_cross(renderer.styles())
        };

        if renderer.is_tty() {
            entry.bar.set_style(styles::style_branch_done());
            set_duration_prefix(&entry.bar, Some(duration_ms));
            entry
                .bar
                .finish_with_message(format!("{glyph} {}", entry.display_name));
        } else {
            renderer.print_line(
                8,
                &format!("{glyph} {branch}  {}", format_duration_ms(duration_ms)),
            );
        }
    }

    pub(super) fn on_assistant_message(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        model: &str,
    ) {
        if let Some(counts) = self.stage_counts.get_mut(stage_node_id) {
            counts.0 += 1;
        }

        if let Some(stage) = self.active_stages.get_mut(stage_node_id) {
            if !stage.has_model {
                stage.has_model = true;
                let suffix = format!(" {}", renderer.styles().dim.apply_to(format!("[{model}]")));
                stage.display_name.push_str(&suffix);
                stage.spinner.set_message(stage.display_name.clone());
            }
        }
    }

    pub(super) fn on_tool_call_started(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        tool_name: &str,
        tool_call_id: &str,
        arguments: &serde_json::Value,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let display_name = self.tool_display_name(renderer, tool_name, arguments);
        let Some(stage) = self.active_stages.get_mut(stage_node_id) else {
            return;
        };

        if !self.verbose && stage.tool_calls.len() >= MAX_TOOL_CALLS {
            let evict_idx = stage
                .tool_calls
                .iter()
                .position(|entry| !matches!(entry.status, ToolCallStatus::Running))
                .unwrap_or(0);
            if let Some(evicted) = stage.tool_calls.remove(evict_idx) {
                evicted.bar.finish_and_clear();
            }
        }

        let bar = renderer.insert_after(stage.last_bar());
        bar.set_style(styles::style_tool_running());
        bar.set_message(display_name.clone());
        if renderer.is_tty() {
            bar.enable_steady_tick(Duration::from_millis(100));
        }
        stage.tool_calls.push_back(ToolCallEntry {
            display_name,
            tool_call_id: tool_call_id.to_string(),
            status: ToolCallStatus::Running,
            bar,
            is_branch: false,
            started_at: timestamp,
        });
    }

    pub(super) fn on_tool_call_completed(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        tool_call_id: &str,
        is_error: bool,
        duration_ms: Option<u64>,
        timestamp: Option<DateTime<Utc>>,
    ) {
        if let Some(counts) = self.stage_counts.get_mut(stage_node_id) {
            counts.1 += 1;
        }

        let Some(stage) = self.active_stages.get_mut(stage_node_id) else {
            return;
        };
        let Some(entry) = stage
            .tool_calls
            .iter_mut()
            .find(|entry| entry.tool_call_id == tool_call_id)
        else {
            return;
        };

        let glyph = if is_error {
            styles::red_cross(renderer.styles())
        } else {
            styles::green_check(renderer.styles())
        };
        entry.status = if is_error {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Succeeded
        };
        if renderer.is_tty() {
            entry.bar.set_style(styles::style_tool_done());
            let computed_duration_ms = duration_ms.or_else(|| {
                entry
                    .started_at
                    .zip(timestamp)
                    .and_then(|(started_at, completed_at)| {
                        u64::try_from(
                            completed_at
                                .signed_duration_since(started_at)
                                .num_milliseconds(),
                        )
                        .ok()
                    })
            });
            set_duration_prefix(&entry.bar, computed_duration_ms);
            entry
                .bar
                .finish_with_message(format!("{glyph} {}", entry.display_name));
        }
    }

    pub(super) fn on_context_window_warning(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        usage_percent: u64,
    ) {
        if !self.verbose {
            return;
        }

        self.insert_info_line_for_stage(
            renderer,
            stage_node_id,
            &format!(
                "{} context window: {usage_percent}% used",
                styles::warning_glyph(renderer.styles())
            ),
        );
    }

    pub(super) fn on_compaction_started(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
    ) {
        if !renderer.is_tty() {
            return;
        }
        let Some(stage) = self.active_stages.get_mut(stage_node_id) else {
            return;
        };

        if let Some(old) = stage.compaction_bar.take() {
            old.finish_and_clear();
        }
        let bar = renderer.insert_after(stage.last_bar());
        bar.set_style(styles::style_tool_running());
        bar.set_message("\u{27f3} compacting context\u{2026}");
        bar.enable_steady_tick(Duration::from_millis(100));
        stage.compaction_bar = Some(bar);
    }

    pub(super) fn on_compaction_completed(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        original_turn_count: u64,
        preserved_turn_count: u64,
        tracked_file_count: u64,
    ) {
        let message = format!(
            "\u{27f3} compaction: {original_turn_count} \u{2192} {preserved_turn_count} turns, {tracked_file_count} files"
        );

        if renderer.is_tty() {
            if let Some(bar) = self
                .active_stages
                .get_mut(stage_node_id)
                .and_then(|stage| stage.compaction_bar.take())
            {
                bar.set_style(styles::style_tool_done());
                bar.finish_with_message(message);
            } else {
                self.insert_info_line_for_stage(renderer, stage_node_id, &message);
            }
        } else {
            renderer.print_line(6, &message);
        }
    }

    pub(super) fn on_llm_retry(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        model: &str,
        attempt: u64,
        delay_ms: u64,
        error: &str,
    ) {
        if !self.verbose {
            return;
        }

        self.insert_info_line_for_stage(
            renderer,
            stage_node_id,
            &format!(
                "{} retry: {model} attempt {attempt} ({error}, delay {})",
                styles::warning_glyph(renderer.styles()),
                format_duration_ms(delay_ms)
            ),
        );
    }

    pub(super) fn on_subagent_spawned(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        agent_id: &str,
        task: &str,
    ) {
        if !self.verbose {
            return;
        }

        self.insert_subagent_line_for_stage(
            renderer,
            stage_node_id,
            &renderer
                .styles()
                .dim
                .apply_to(format!(
                    "\u{25b8} subagent[{agent_id}] \"{}\"",
                    styles::truncate(task, 50)
                ))
                .to_string(),
        );
    }

    pub(super) fn on_subagent_completed(
        &mut self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        agent_id: &str,
        success: bool,
        turns_used: u64,
    ) {
        if !self.verbose {
            return;
        }

        let glyph = if success {
            styles::green_check(renderer.styles())
        } else {
            styles::red_cross(renderer.styles())
        };
        self.insert_subagent_line_for_stage(
            renderer,
            stage_node_id,
            &format!("{glyph} subagent[{agent_id}] ({turns_used} turns)"),
        );
    }

    pub(super) fn on_retro_started(&mut self, renderer: &ProgressRenderer) {
        self.on_stage_started(renderer, "retro", "Retro", None);
    }

    pub(super) fn on_retro_completed(&mut self, renderer: &ProgressRenderer, duration_ms: u64) {
        self.finish_stage(
            renderer,
            "retro",
            "Retro",
            &styles::green_check(renderer.styles()),
            &format_duration_ms(duration_ms),
        );
    }

    pub(super) fn on_retro_failed(&mut self, renderer: &ProgressRenderer, duration_ms: u64) {
        self.finish_stage(
            renderer,
            "retro",
            "Retro",
            &styles::red_cross(renderer.styles()),
            &format_duration_ms(duration_ms),
        );
    }

    fn finish_stage(
        &mut self,
        renderer: &ProgressRenderer,
        node_id: &str,
        name: &str,
        glyph: &str,
        prefix: &str,
    ) {
        let Some(stage) = self.active_stages.remove(node_id) else {
            if !renderer.is_tty() {
                Self::print_plain_stage_completion(renderer, name, glyph, prefix);
            }
            return;
        };

        if let Some(bar) = stage.compaction_bar {
            bar.finish_and_clear();
        }
        for entry in &stage.tool_calls {
            if entry.is_branch || self.verbose {
                entry.bar.abandon();
            } else {
                entry.bar.finish_and_clear();
            }
        }

        if renderer.is_tty() {
            stage.spinner.set_style(styles::style_stage_done());
            stage.spinner.set_prefix(prefix.to_string());
            stage
                .spinner
                .finish_with_message(format!("{glyph} {}", stage.display_name));
        } else {
            Self::print_plain_stage_completion(renderer, name, glyph, prefix);
        }
    }

    fn print_plain_stage_completion(
        renderer: &ProgressRenderer,
        name: &str,
        glyph: &str,
        prefix: &str,
    ) {
        if prefix.is_empty() {
            renderer.print_line(4, &format!("{glyph} {name}"));
        } else {
            renderer.print_line(4, &format!("{glyph} {name}  {prefix}"));
        }
    }

    fn insert_global_info_line(renderer: &ProgressRenderer, message: &str) {
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_static_dim());
            bar.finish_with_message(message.to_string());
        } else {
            renderer.print_line(4, message);
        }
    }

    fn insert_info_line_for_stage(
        &self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        message: &str,
    ) {
        if renderer.is_tty() {
            let bar = if let Some(stage) = self.active_stages.get(stage_node_id) {
                renderer.insert_after(stage.last_bar())
            } else {
                renderer.add_spinner()
            };
            bar.set_style(styles::style_tool_done());
            bar.finish_with_message(message.to_string());
        } else {
            renderer.print_line(6, message);
        }
    }

    fn insert_subagent_line_for_stage(
        &self,
        renderer: &ProgressRenderer,
        stage_node_id: &str,
        message: &str,
    ) {
        if renderer.is_tty() {
            let bar = if let Some(stage) = self.active_stages.get(stage_node_id) {
                renderer.insert_after(stage.last_bar())
            } else {
                renderer.add_spinner()
            };
            bar.set_style(styles::style_subagent_info());
            bar.finish_with_message(message.to_string());
        } else {
            renderer.print_line(8, message);
        }
    }

    fn tool_display_name(
        &self,
        renderer: &ProgressRenderer,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> String {
        let arg = |key: &str| arguments.get(key).and_then(serde_json::Value::as_str);
        let working_directory = self.working_directory.as_deref();
        let path_arg = || {
            arg("path")
                .or_else(|| arg("file_path"))
                .map(|path| styles::truncate(&styles::shorten_path(path, working_directory), 60))
        };

        let detail = match tool_name {
            "bash" | "shell" | "execute_command" => {
                arg("command").map(|command| styles::truncate(command, 60))
            }
            "glob" => arg("pattern").map(String::from),
            "grep" | "ripgrep" => arg("pattern").map(|pattern| styles::truncate(pattern, 40)),
            "read_file" | "read" | "write_file" | "write" | "create_file" | "edit_file"
            | "edit" | "list_dir" => path_arg(),
            "web_search" => arg("query").map(|query| styles::truncate(query, 60)),
            "web_fetch" => arg("url").map(|url| styles::truncate(url, 60)),
            "spawn_agent" => arg("task").map(|task| styles::truncate(task, 60)),
            "wait" | "send_input" | "close_agent" => arg("agent_id").map(String::from),
            "use_skill" => arg("skill_name").map(String::from),
            "apply_patch" => Some("...".to_string()),
            "read_many_files" => arguments
                .get("paths")
                .and_then(serde_json::Value::as_array)
                .map(|paths| format!("{} files", paths.len())),
            _ => None,
        };

        match detail {
            Some(detail) => format!(
                "{tool_name}{}",
                renderer.styles().dim.apply_to(format!("({detail})"))
            ),
            None => tool_name.to_string(),
        }
    }
}

fn set_duration_prefix(bar: &ProgressBar, duration_ms: Option<u64>) {
    let prefix = duration_ms.map_or_else(
        || styles::format_duration_short(bar.elapsed()),
        |duration_ms| styles::format_duration_short(Duration::from_millis(duration_ms)),
    );
    bar.set_prefix(prefix);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::run::run_progress::renderer::ProgressRenderer;

    #[test]
    fn tool_display_name_shortens_paths_relative_to_working_directory() {
        let renderer = ProgressRenderer::new_plain(Box::new(std::io::sink()), false);
        let mut stage = StageDisplay::new(false);
        stage.set_working_directory("/workspace".into());

        let display_name = stage.tool_display_name(
            &renderer,
            "read_file",
            &serde_json::json!({"file_path": "/workspace/src/main.rs"}),
        );

        assert_eq!(display_name, "read_file(src/main.rs)");
    }
}
