use std::io::Write;
use std::sync::Mutex;

use fabro_util::terminal::Styles;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget};

use super::styles;

enum RendererInner {
    Tty { multi: MultiProgress },
    Plain { out: Mutex<Box<dyn Write + Send>> },
}

pub(super) struct ProgressRenderer {
    inner: RendererInner,
    styles: Styles,
}

impl ProgressRenderer {
    pub(super) fn new_tty() -> Self {
        Self {
            inner: RendererInner::Tty {
                multi: MultiProgress::new(),
            },
            styles: Styles::new(console::colors_enabled_stderr()),
        }
    }

    pub(super) fn new_plain(out: Box<dyn Write + Send>, colors: bool) -> Self {
        Self {
            inner: RendererInner::Plain {
                out: Mutex::new(out),
            },
            styles: Styles::new(colors),
        }
    }

    pub(super) fn add_spinner(&self) -> ProgressBar {
        match &self.inner {
            RendererInner::Tty { multi } => multi.add(ProgressBar::new_spinner()),
            RendererInner::Plain { .. } => ProgressBar::hidden(),
        }
    }

    pub(super) fn insert_after(&self, after: &ProgressBar) -> ProgressBar {
        match &self.inner {
            RendererInner::Tty { multi } => multi.insert_after(after, ProgressBar::new_spinner()),
            RendererInner::Plain { .. } => ProgressBar::hidden(),
        }
    }

    pub(super) fn insert_before(&self, before: &ProgressBar) -> ProgressBar {
        match &self.inner {
            RendererInner::Tty { multi } => multi.insert_before(before, ProgressBar::new_spinner()),
            RendererInner::Plain { .. } => ProgressBar::hidden(),
        }
    }

    pub(super) fn print_line(&self, indent: usize, message: &str) {
        if let RendererInner::Plain { out } = &self.inner {
            let mut out = out.lock().expect("plain renderer lock poisoned");
            let _ = writeln!(out, "{}{message}", " ".repeat(indent));
        }
    }

    pub(super) fn is_tty(&self) -> bool {
        matches!(self.inner, RendererInner::Tty { .. })
    }

    pub(super) fn styles(&self) -> &Styles {
        &self.styles
    }

    pub(super) fn hide(&self) {
        if let RendererInner::Tty { multi } = &self.inner {
            multi.set_draw_target(ProgressDrawTarget::hidden());
        }
    }

    pub(super) fn show(&self) {
        if let RendererInner::Tty { multi } = &self.inner {
            multi.set_draw_target(ProgressDrawTarget::stderr());
        }
    }

    pub(super) fn finish(&self) {
        if let RendererInner::Tty { multi } = &self.inner {
            let sep = multi.add(ProgressBar::new_spinner());
            sep.set_style(styles::style_empty());
            sep.finish();
            multi.set_draw_target(ProgressDrawTarget::hidden());
        }
    }
}
