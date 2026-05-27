use std::time::Duration;

use indicatif::ProgressBar;

use super::renderer::ProgressRenderer;
use super::styles;
use crate::shared::format_duration_ms;

pub(super) struct SetupDisplay {
    verbose: bool,
    current_provider: Option<String>,
    pub(super) sandbox_bar: Option<ProgressBar>,
    pub(super) setup_bar: Option<ProgressBar>,
    pub(super) setup_command_count: u64,
    pub(super) cli_ensure_bar: Option<ProgressBar>,
}

impl SetupDisplay {
    pub(super) fn new(verbose: bool) -> Self {
        Self {
            verbose,
            current_provider: None,
            sandbox_bar: None,
            setup_bar: None,
            setup_command_count: 0,
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
        if let Some(bar) = self.cli_ensure_bar.take() {
            bar.finish_and_clear();
        }
    }

    pub(super) fn on_sandbox_initializing(&mut self, renderer: &ProgressRenderer, provider: &str) {
        self.current_provider = Some(provider.to_string());
        if renderer.is_tty() {
            let bar = renderer.add_spinner();
            bar.set_style(styles::style_header_running());
            bar.set_message(initializing_message(provider));
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

    pub(super) fn on_sandbox_failed(
        &mut self,
        renderer: &ProgressRenderer,
        provider: &str,
        error: &str,
    ) {
        if renderer.is_tty() {
            let bar = self
                .sandbox_bar
                .take()
                .unwrap_or_else(|| renderer.add_spinner());
            bar.set_style(styles::style_header_failed());
            bar.finish_with_message(format!("Sandbox: {provider} failed: {error}"));
        } else {
            renderer.print_line(4, &format!("Sandbox: {provider} failed: {error}"));
        }
    }

    pub(super) fn on_snapshot_pulling(&self, renderer: &ProgressRenderer, name: &str) {
        if renderer.is_tty() {
            if let Some(bar) = self.sandbox_bar.as_ref() {
                bar.set_message(format!("Pulling {name}..."));
            }
        } else {
            renderer.print_line(4, &format!("Sandbox: pulling {name}..."));
        }
    }

    pub(super) fn on_snapshot_creating(&self, renderer: &ProgressRenderer, name: &str) {
        if renderer.is_tty() {
            if let Some(bar) = self.sandbox_bar.as_ref() {
                bar.set_message(format!("Building {name}..."));
            }
        } else {
            renderer.print_line(4, &format!("Sandbox: building {name}..."));
        }
    }

    pub(super) fn on_snapshot_ready(
        &self,
        renderer: &ProgressRenderer,
        _name: &str,
        _duration_ms: u64,
    ) {
        if renderer.is_tty() {
            if let Some(bar) = self.sandbox_bar.as_ref() {
                let provider = self.current_provider.as_deref().unwrap_or("sandbox");
                bar.set_style(styles::style_header_running());
                bar.set_message(initializing_message(provider));
            }
        }
    }

    pub(super) fn on_snapshot_failed(&self, renderer: &ProgressRenderer, name: &str, error: &str) {
        let message = format!("Snapshot {name} failed: {error}");
        if renderer.is_tty() {
            if let Some(bar) = self.sandbox_bar.as_ref() {
                bar.set_message(message);
            }
        } else {
            renderer.print_line(4, &format!("Sandbox: {message}"));
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
}

fn initializing_message(provider: &str) -> String {
    if provider == "sandbox" {
        "Initializing sandbox...".to_string()
    } else {
        format!("Initializing {provider} sandbox...")
    }
}
