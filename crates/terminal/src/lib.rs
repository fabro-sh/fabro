use std::io::IsTerminal;

/// Pre-resolved ANSI escape codes for styled terminal output.
/// All fields are empty strings when color is disabled (non-TTY or `NO_COLOR`).
pub struct Styles {
    pub bold: &'static str,
    pub dim: &'static str,
    pub cyan: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub red: &'static str,
    pub reset: &'static str,
}

// SAFETY: Styles contains only `&'static str` fields, which are inherently Send + Sync.
unsafe impl Send for Styles {}
unsafe impl Sync for Styles {}

impl Styles {
    #[must_use]
    pub const fn new(use_color: bool) -> Self {
        if use_color {
            Self {
                bold: "\x1b[1m",
                dim: "\x1b[2m",
                cyan: "\x1b[36m",
                green: "\x1b[32m",
                yellow: "\x1b[33m",
                red: "\x1b[31m",
                reset: "\x1b[0m",
            }
        } else {
            Self {
                bold: "",
                dim: "",
                cyan: "",
                green: "",
                yellow: "",
                red: "",
                reset: "",
            }
        }
    }

    /// Create styles based on whether stderr is a TTY.
    /// Respects `NO_COLOR` environment variable.
    #[must_use]
    pub fn detect_stderr() -> Self {
        let use_color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self::new(use_color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_with_color() {
        let s = Styles::new(true);
        assert_eq!(s.bold, "\x1b[1m");
        assert_eq!(s.dim, "\x1b[2m");
        assert_eq!(s.cyan, "\x1b[36m");
        assert_eq!(s.green, "\x1b[32m");
        assert_eq!(s.yellow, "\x1b[33m");
        assert_eq!(s.red, "\x1b[31m");
        assert_eq!(s.reset, "\x1b[0m");
    }

    #[test]
    fn styles_without_color() {
        let s = Styles::new(false);
        assert!(s.bold.is_empty());
        assert!(s.dim.is_empty());
        assert!(s.cyan.is_empty());
        assert!(s.green.is_empty());
        assert!(s.yellow.is_empty());
        assert!(s.red.is_empty());
        assert!(s.reset.is_empty());
    }

    static NO_COLOR: Styles = Styles::new(false);

    #[test]
    fn no_color_static_is_empty() {
        assert!(NO_COLOR.bold.is_empty());
        assert!(NO_COLOR.reset.is_empty());
    }
}
