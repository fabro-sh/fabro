#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Printer {
    /// Suppresses all output.
    Silent,
    /// Suppresses most output, but preserves "important" stdout.
    Quiet,
    /// Prints to standard streams.
    Default,
    /// Prints all output, including debug messages.
    Verbose,
}

impl Printer {
    /// Build from CLI flags. `quiet` wins if both are set.
    pub fn from_flags(quiet: bool, verbose: bool) -> Self {
        match (quiet, verbose) {
            (true, _) => Self::Quiet,
            (_, true) => Self::Verbose,
            _ => Self::Default,
        }
    }

    /// Stdout writer — enabled for Default/Verbose.
    pub fn stdout(self) -> Stdout {
        match self {
            Self::Silent | Self::Quiet => Stdout::Disabled,
            Self::Default | Self::Verbose => Stdout::Enabled,
        }
    }

    /// Stdout for important messages — enabled for Quiet/Default/Verbose.
    pub fn stdout_important(self) -> Stdout {
        match self {
            Self::Silent => Stdout::Disabled,
            Self::Quiet | Self::Default | Self::Verbose => Stdout::Enabled,
        }
    }

    /// Stderr writer — enabled for Default/Verbose.
    pub fn stderr(self) -> Stderr {
        match self {
            Self::Silent | Self::Quiet => Stderr::Disabled,
            Self::Default | Self::Verbose => Stderr::Enabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdout {
    Enabled,
    Disabled,
}

impl std::fmt::Write for Stdout {
    #[allow(clippy::print_stdout)]
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        match self {
            Self::Enabled => print!("{s}"),
            Self::Disabled => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stderr {
    Enabled,
    Disabled,
}

impl std::fmt::Write for Stderr {
    #[allow(clippy::print_stderr)]
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        match self {
            Self::Enabled => eprint!("{s}"),
            Self::Disabled => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_flags_default() {
        assert_eq!(Printer::from_flags(false, false), Printer::Default);
    }

    #[test]
    fn from_flags_quiet() {
        assert_eq!(Printer::from_flags(true, false), Printer::Quiet);
    }

    #[test]
    fn from_flags_verbose() {
        assert_eq!(Printer::from_flags(false, true), Printer::Verbose);
    }

    #[test]
    fn from_flags_quiet_wins() {
        assert_eq!(Printer::from_flags(true, true), Printer::Quiet);
    }

    #[test]
    fn stdout_silent() {
        assert_eq!(Printer::Silent.stdout(), Stdout::Disabled);
    }

    #[test]
    fn stdout_quiet() {
        assert_eq!(Printer::Quiet.stdout(), Stdout::Disabled);
    }

    #[test]
    fn stdout_default() {
        assert_eq!(Printer::Default.stdout(), Stdout::Enabled);
    }

    #[test]
    fn stdout_verbose() {
        assert_eq!(Printer::Verbose.stdout(), Stdout::Enabled);
    }

    #[test]
    fn stdout_important_silent() {
        assert_eq!(Printer::Silent.stdout_important(), Stdout::Disabled);
    }

    #[test]
    fn stdout_important_quiet() {
        assert_eq!(Printer::Quiet.stdout_important(), Stdout::Enabled);
    }

    #[test]
    fn stderr_silent() {
        assert_eq!(Printer::Silent.stderr(), Stderr::Disabled);
    }

    #[test]
    fn stderr_quiet() {
        assert_eq!(Printer::Quiet.stderr(), Stderr::Disabled);
    }

    #[test]
    fn stderr_default() {
        assert_eq!(Printer::Default.stderr(), Stderr::Enabled);
    }

    #[test]
    fn stderr_verbose() {
        assert_eq!(Printer::Verbose.stderr(), Stderr::Enabled);
    }
}
