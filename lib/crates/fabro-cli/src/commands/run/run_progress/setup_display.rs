use std::time::Duration;

use indicatif::ProgressBar;

use super::renderer::ProgressRenderer;
use super::styles;
use crate::shared::format_duration_ms;

pub(super) struct SetupDisplay {
    verbose: bool,
    pub(super) sandbox_bar: Option<ProgressBar>,
    pub(super) setup_bar: Option<ProgressBar>,
    pub(super) setup_command_count: u64,
    pub(super) devcontainer_bar: Option<ProgressBar>,
    pub(super) devcontainer_command_count: u64,
    pub(super) cli_ensure_bar: Option<ProgressBar>,
}

impl SetupDisplay {
    pub(super) fn new(verbose: bool) -> Self {
        Self {
            verbose,
            sandbox_bar: None,
            setup_bar: None,
            setup_command_count: 0,
            devcontainer_bar: None,
            devcontainer_command_count: 0,
            cli_ensure_bar: None,
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(bar) = self.sandbox_bar.take() {
            bar.finish_and_clear();
        }
        if let Some(bar) = self.setup_bar.take() {
            bar.finish_and_clear();
        }
        if let Some(bar) = self.devcontainer_bar.take() {
            bar.finish_and_clear();
        }
        if let Some(bar) = self.cli_ensure_bar.take() {
            bar.finish_and_clear();
        }
    }

    pub(super) fn on_sandbox_initializing(&mut self, renderer: &ProgressRenderer, provider: &str) {
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_header_running());
            bar.set_message(format!("Initializing {provider} sandbox..."));
            bar.enable_steady_tick(Duration::from_millis(100));
            self.sandbox_bar = Some(bar);
        }
    }

    pub(super) fn on_sandbox_ready(
        &mut self,
        renderer: &ProgressRenderer,
        provider: &str,
        duration_ms: u64,
        name: Option<&str>,
        cpu: Option<f64>,
        memory: Option<f64>,
        url: Option<&str>,
    ) {
        let dur = format_duration_ms(duration_ms);
        let detail = match (name, cpu, memory) {
            (Some(name), Some(cpu), Some(memory)) => Some(format!(
                "{name} ({} cpu, {} GB)",
                styles::format_number(cpu),
                styles::format_number(memory)
            )),
            (Some(name), _, _) => Some(name.to_string()),
            _ => None,
        };

        if renderer.is_tty() {
            let display_provider = match url {
                Some(url) => styles::terminal_hyperlink(url, provider),
                None => provider.to_string(),
            };

            if let Some(bar) = self.sandbox_bar.take() {
                bar.set_style(styles::style_header_done());
                bar.set_prefix(dur);
                bar.finish_with_message(format!("Sandbox: {display_provider}"));
                if let Some(detail) = detail {
                    let detail_bar = renderer.insert_after(&bar);
                    detail_bar.set_style(styles::style_sandbox_detail());
                    detail_bar.finish_with_message(detail);
                }
            }
        } else {
            renderer.print_line(4, &format!("Sandbox: {provider} (ready in {dur})"));
            if let Some(detail) = detail {
                renderer.print_line(13, &detail);
            }
        }
    }

    pub(super) fn on_ssh_access_ready(renderer: &ProgressRenderer, ssh_command: &str) {
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_sandbox_detail());
            bar.finish_with_message(ssh_command.to_string());
        } else {
            renderer.print_line(13, ssh_command);
        }
    }

    pub(super) fn on_setup_started(&mut self, renderer: &ProgressRenderer, command_count: u64) {
        self.setup_command_count = command_count;
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_header_running());
            bar.set_message(format!(
                "Setup: {command_count} command{}...",
                if command_count == 1 { "" } else { "s" }
            ));
            bar.enable_steady_tick(Duration::from_millis(100));
            self.setup_bar = Some(bar);
        }
    }

    pub(super) fn on_setup_completed(&mut self, renderer: &ProgressRenderer, duration_ms: u64) {
        let dur = format_duration_ms(duration_ms);
        let suffix = if self.setup_command_count == 1 {
            ""
        } else {
            "s"
        };

        if renderer.is_tty() {
            if let Some(bar) = self.setup_bar.take() {
                bar.set_style(styles::style_header_done());
                bar.set_prefix(dur);
                bar.finish_with_message(format!(
                    "Setup: {} command{suffix}",
                    self.setup_command_count
                ));
            }
        } else {
            renderer.print_line(
                4,
                &format!(
                    "Setup: {} command{suffix} ({dur})",
                    self.setup_command_count
                ),
            );
        }
    }

    pub(super) fn on_setup_command_completed(
        &self,
        renderer: &ProgressRenderer,
        command: &str,
        command_index: u64,
        exit_code: i64,
        duration_ms: u64,
    ) {
        if !self.verbose {
            return;
        }

        let glyph = if exit_code == 0 {
            styles::green_check(renderer.styles())
        } else {
            styles::red_cross(renderer.styles())
        };
        let msg = format!(
            "{glyph} [{}/{}] {}",
            command_index + 1,
            self.setup_command_count,
            styles::truncate(command, 60)
        );
        let dur = format_duration_ms(duration_ms);

        if renderer.is_tty() {
            let bar = match &self.setup_bar {
                Some(setup_bar) => renderer.insert_before(setup_bar),
                None => renderer.add_spinner(),
            };
            bar.set_style(styles::style_tool_done());
            bar.set_prefix(dur);
            bar.finish_with_message(msg);
        } else {
            renderer.print_line(6, &format!("{msg}  {dur}"));
        }
    }

    pub(super) fn on_cli_ensure_started(&mut self, renderer: &ProgressRenderer, cli_name: &str) {
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_header_running());
            bar.set_message(format!("CLI: ensuring {cli_name}..."));
            bar.enable_steady_tick(Duration::from_millis(100));
            self.cli_ensure_bar = Some(bar);
        }
    }

    pub(super) fn on_cli_ensure_completed(
        &mut self,
        renderer: &ProgressRenderer,
        cli_name: &str,
        already_installed: bool,
        duration_ms: u64,
    ) {
        let status = if already_installed {
            "found"
        } else {
            "installed"
        };
        let dur = format_duration_ms(duration_ms);

        if renderer.is_tty() {
            if let Some(bar) = self.cli_ensure_bar.take() {
                bar.set_style(styles::style_header_done());
                bar.set_prefix(dur);
                bar.finish_with_message(format!("CLI: {cli_name} ({status})"));
            }
        } else {
            renderer.print_line(4, &format!("CLI: {cli_name} ({status}, {dur})"));
        }
    }

    pub(super) fn on_cli_ensure_failed(&mut self, renderer: &ProgressRenderer, cli_name: &str) {
        let message = format!(
            "{} CLI: {cli_name} install failed",
            styles::red_cross(renderer.styles())
        );
        if renderer.is_tty() {
            if let Some(bar) = self.cli_ensure_bar.take() {
                bar.set_style(styles::style_header_done());
                bar.finish_with_message(message);
            }
        } else {
            renderer.print_line(4, &message);
        }
    }

    pub(super) fn on_devcontainer_resolved(
        renderer: &ProgressRenderer,
        dockerfile_lines: u64,
        environment_count: u64,
        lifecycle_command_count: u64,
        workspace_folder: &str,
    ) {
        let detail = format!(
            "{dockerfile_lines} Dockerfile lines, {environment_count} env vars, \
             {lifecycle_command_count} lifecycle cmds, {workspace_folder}"
        );

        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_header_done());
            bar.finish_with_message("Devcontainer: resolved".to_string());
            let detail_bar = renderer.insert_after(&bar);
            detail_bar.set_style(styles::style_sandbox_detail());
            detail_bar.finish_with_message(detail);
        } else {
            renderer.print_line(4, "Devcontainer: resolved");
            renderer.print_line(13, &detail);
        }
    }

    pub(super) fn on_devcontainer_lifecycle_started(
        &mut self,
        renderer: &ProgressRenderer,
        phase: &str,
        command_count: u64,
    ) {
        self.devcontainer_command_count = command_count;

        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_header_running());
            bar.set_message(format!(
                "Running devcontainer {phase} ({command_count} commands)..."
            ));
            bar.enable_steady_tick(Duration::from_millis(100));
            self.devcontainer_bar = Some(bar);
        } else {
            renderer.print_line(
                4,
                &format!("Running devcontainer {phase} ({command_count} commands)..."),
            );
        }
    }

    pub(super) fn on_devcontainer_lifecycle_completed(
        &mut self,
        renderer: &ProgressRenderer,
        phase: &str,
        duration_ms: u64,
    ) {
        let dur = format_duration_ms(duration_ms);

        if renderer.is_tty() {
            if let Some(bar) = self.devcontainer_bar.take() {
                bar.set_style(styles::style_header_done());
                bar.set_prefix(dur);
                bar.finish_with_message(format!("Devcontainer: {phase}"));
            }
        } else {
            renderer.print_line(4, &format!("Devcontainer: {phase} ({dur})"));
        }
    }

    pub(super) fn on_devcontainer_lifecycle_failed(
        &mut self,
        renderer: &ProgressRenderer,
        phase: &str,
        command: &str,
        exit_code: i64,
        stderr: &str,
    ) {
        if let Some(bar) = self.devcontainer_bar.take() {
            bar.abandon();
        }

        let summary = if stderr.len() > 120 {
            &stderr[..120]
        } else {
            stderr
        };
        let message = format!(
            "{} Devcontainer {phase} command failed (exit {exit_code}): {command}\n         {summary}",
            renderer.styles().red.apply_to("Error:")
        );

        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_static_dim());
            bar.finish_with_message(message);
        } else {
            renderer.print_line(4, &message);
        }
    }

    pub(super) fn on_devcontainer_lifecycle_command_completed(
        &self,
        renderer: &ProgressRenderer,
        command: &str,
        command_index: u64,
        exit_code: i64,
        duration_ms: u64,
    ) {
        if !self.verbose {
            return;
        }

        let glyph = if exit_code == 0 {
            styles::green_check(renderer.styles())
        } else {
            styles::red_cross(renderer.styles())
        };
        let msg = format!(
            "{glyph} [{}/{}] {}",
            command_index + 1,
            self.devcontainer_command_count,
            styles::truncate(command, 60)
        );
        let dur = format_duration_ms(duration_ms);

        if renderer.is_tty() {
            let bar = match &self.devcontainer_bar {
                Some(devcontainer_bar) => renderer.insert_before(devcontainer_bar),
                None => renderer.add_spinner(),
            };
            bar.set_style(styles::style_tool_done());
            bar.set_prefix(dur);
            bar.finish_with_message(msg);
        } else {
            renderer.print_line(6, &format!("{msg}  {dur}"));
        }
    }
}
