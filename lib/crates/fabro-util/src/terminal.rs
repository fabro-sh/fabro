use console::Style;

/// Pre-built [`console::Style`] instances for styled terminal output.
/// Each style is forced on/off based on the `use_color` flag passed to
/// [`Styles::new`].
pub struct Styles {
    pub use_color: bool,
    pub bold: Style,
    pub dim: Style,
    pub cyan: Style,
    pub green: Style,
    pub yellow: Style,
    pub red: Style,
    pub magenta: Style,
    pub underline: Style,
    pub bold_dim: Style,
    pub bold_cyan: Style,
    pub bold_green: Style,
    pub bold_red: Style,
}

impl Styles {
    #[must_use]
    pub fn new(use_color: bool) -> Self {
        Self {
            use_color,
            bold: Style::new().bold().force_styling(use_color),
            dim: Style::new().dim().force_styling(use_color),
            cyan: Style::new().cyan().force_styling(use_color),
            green: Style::new().green().force_styling(use_color),
            yellow: Style::new().yellow().force_styling(use_color),
            red: Style::new().red().force_styling(use_color),
            magenta: Style::new().magenta().force_styling(use_color),
            underline: Style::new().underlined().force_styling(use_color),
            bold_dim: Style::new().bold().dim().force_styling(use_color),
            bold_cyan: Style::new().bold().cyan().force_styling(use_color),
            bold_green: Style::new().bold().green().force_styling(use_color),
            bold_red: Style::new().bold().red().force_styling(use_color),
        }
    }

    /// Render Markdown text with terminal formatting (bold, tables, headers).
    /// Returns plain text when color is disabled.
    #[must_use]
    pub fn render_markdown(&self, text: &str) -> String {
        if !self.use_color {
            return text.to_string();
        }
        termimad::term_text(text).to_string()
    }

    /// Render Markdown wrapped to a specific width (in columns).
    /// Returns plain text when color is disabled.
    #[must_use]
    pub fn render_markdown_width(&self, text: &str, width: usize) -> String {
        if !self.use_color {
            return text.to_string();
        }
        termimad::MadSkin::default()
            .text(text, Some(width))
            .to_string()
    }

    /// Return the terminal width in columns, or 80 if it cannot be detected.
    #[must_use]
    pub fn terminal_width() -> usize {
        let (w, _) = termimad::terminal_size();
        w as usize
    }

    /// Create styles based on whether stderr is a TTY.
    /// Respects `NO_COLOR` environment variable.
    #[must_use]
    pub fn detect_stderr() -> Self {
        Self::new(console::colors_enabled_stderr())
    }

    /// Create styles based on whether stdout is a TTY.
    /// Respects `NO_COLOR` environment variable.
    #[must_use]
    pub fn detect_stdout() -> Self {
        Self::new(console::colors_enabled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_with_color_produces_ansi() {
        let s = Styles::new(true);
        let output = format!("{}", s.bold.apply_to("text"));
        assert!(output.contains("\x1b["), "bold should contain ANSI codes");
        assert!(output.contains("text"));

        let output = format!("{}", s.green.apply_to("ok"));
        assert!(output.contains("\x1b["), "green should contain ANSI codes");
        assert!(output.contains("ok"));
    }

    #[test]
    fn styles_without_color_produces_plain_text() {
        let s = Styles::new(false);
        assert_eq!(format!("{}", s.bold.apply_to("text")), "text");
        assert_eq!(format!("{}", s.dim.apply_to("text")), "text");
        assert_eq!(format!("{}", s.cyan.apply_to("text")), "text");
        assert_eq!(format!("{}", s.green.apply_to("text")), "text");
        assert_eq!(format!("{}", s.yellow.apply_to("text")), "text");
        assert_eq!(format!("{}", s.red.apply_to("text")), "text");
        assert_eq!(format!("{}", s.magenta.apply_to("text")), "text");
        assert_eq!(format!("{}", s.underline.apply_to("text")), "text");
    }

    #[test]
    fn combined_styles_work() {
        let s = Styles::new(true);
        let output = format!("{}", s.bold_dim.apply_to("header"));
        assert!(
            output.contains("\x1b["),
            "bold_dim should contain ANSI codes"
        );
        assert!(output.contains("header"));

        let output = format!("{}", s.bold_cyan.apply_to("tool"));
        assert!(
            output.contains("\x1b["),
            "bold_cyan should contain ANSI codes"
        );
        assert!(output.contains("tool"));

        let output = format!("{}", s.bold_green.apply_to("pass"));
        assert!(
            output.contains("\x1b["),
            "bold_green should contain ANSI codes"
        );
        assert!(output.contains("pass"));

        let output = format!("{}", s.bold_red.apply_to("fail"));
        assert!(
            output.contains("\x1b["),
            "bold_red should contain ANSI codes"
        );
        assert!(output.contains("fail"));

        let output = format!("{}", s.magenta.apply_to("medium"));
        assert!(
            output.contains("\x1b["),
            "magenta should contain ANSI codes"
        );
        assert!(output.contains("medium"));

        let output = format!("{}", s.underline.apply_to("path"));
        assert!(
            output.contains("\x1b["),
            "underline should contain ANSI codes"
        );
        assert!(output.contains("path"));
    }

    #[test]
    fn no_color_lazy_is_plain() {
        let styles = Styles::new(false);
        assert_eq!(format!("{}", styles.bold.apply_to("x")), "x");
    }
}
