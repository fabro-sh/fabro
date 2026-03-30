use std::path::Path;

use fabro_workflow::event::RunNoticeLevel;

use super::renderer::ProgressRenderer;
use super::styles;
use crate::shared::{format_duration_ms, tilde_path};

pub(super) struct InfoDisplay {
    verbose: bool,
}

impl InfoDisplay {
    pub(super) fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    pub(super) fn show_worktree(&self, renderer: &ProgressRenderer, path: &Path) {
        self.insert_info_line(renderer, &format!("Worktree: {}", tilde_path(path)));
    }

    pub(super) fn show_base_info(
        &self,
        renderer: &ProgressRenderer,
        branch: Option<&str>,
        sha: &str,
    ) {
        let short_sha = &sha[..sha.len().min(12)];
        let text = match branch {
            Some(branch) => format!("Base: {branch} ({short_sha})"),
            None => format!("Base: {short_sha}"),
        };
        self.insert_info_line(renderer, &text);
    }

    pub(super) fn on_run_notice(
        &self,
        renderer: &ProgressRenderer,
        level: RunNoticeLevel,
        code: &str,
        message: &str,
    ) {
        let styles = renderer.styles();
        let label = match level {
            RunNoticeLevel::Info => styles.bold.apply_to("Info:").to_string(),
            RunNoticeLevel::Warn => styles.yellow.apply_to("Warning:").to_string(),
            RunNoticeLevel::Error => styles.red.apply_to("Error:").to_string(),
        };
        let code_suffix = if code.is_empty() {
            String::new()
        } else {
            format!(" {}", styles.dim.apply_to(format!("[{code}]")))
        };
        self.insert_info_line(renderer, &format!("{label} {message}{code_suffix}"));
    }

    pub(super) fn on_pull_request_created(
        &self,
        renderer: &ProgressRenderer,
        pr_url: &str,
        draft: bool,
    ) {
        let label = if draft { "Draft PR:" } else { "PR:" };
        self.insert_info_line(
            renderer,
            &format!("{} {pr_url}", renderer.styles().bold.apply_to(label)),
        );
    }

    pub(super) fn on_pull_request_failed(&self, renderer: &ProgressRenderer, error: &str) {
        self.insert_info_line(
            renderer,
            &format!("{} {error}", renderer.styles().red.apply_to("PR failed:")),
        );
    }

    pub(super) fn on_edge_selected(
        &self,
        renderer: &ProgressRenderer,
        from_node: &str,
        to_node: &str,
        label: Option<&str>,
        condition: Option<&str>,
    ) {
        if !self.verbose {
            return;
        }

        let detail = if let Some(condition) = condition {
            format!("  [{condition}]")
        } else if let Some(label) = label {
            format!("  \"{label}\"")
        } else {
            String::new()
        };
        self.insert_info_line(
            renderer,
            &format!("\u{2192} {from_node} \u{2192} {to_node}{detail}"),
        );
    }

    pub(super) fn on_loop_restart(
        &self,
        renderer: &ProgressRenderer,
        from_node: &str,
        to_node: &str,
    ) {
        if !self.verbose {
            return;
        }

        self.insert_info_line(
            renderer,
            &format!("\u{21ba} {from_node} \u{2192} {to_node}  (loop restart)"),
        );
    }

    pub(super) fn on_stage_retrying(
        &self,
        renderer: &ProgressRenderer,
        name: &str,
        attempt: u64,
        max_attempts: u64,
        delay_ms: u64,
    ) {
        if !self.verbose {
            return;
        }

        self.insert_info_line(
            renderer,
            &format!(
                "\u{21bb} {name}: retrying (attempt {attempt}/{max_attempts}, delay {})",
                format_duration_ms(delay_ms)
            ),
        );
    }

    fn insert_info_line(&self, renderer: &ProgressRenderer, message: &str) {
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_static_dim());
            bar.finish_with_message(message.to_string());
        } else {
            renderer.print_line(4, message);
        }
    }
}
