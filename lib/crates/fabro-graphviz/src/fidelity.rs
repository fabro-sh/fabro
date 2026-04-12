use std::fmt;
use std::str::FromStr;

/// Fidelity mode controlling how much prior context is provided to LLM
/// sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fidelity {
    /// Complete context, no summarization — sessions share a thread.
    Full,
    /// Minimal: only graph goal and run ID.
    Truncate,
    /// Structured nested-bullet summary (default).
    #[default]
    Compact,
    /// Brief textual summary (~600 token target).
    SummaryLow,
    /// Moderate textual summary (~1500 token target).
    SummaryMedium,
    /// Detailed per-stage Markdown report.
    SummaryHigh,
}

impl Fidelity {
    /// Degrade full fidelity to summary:high (used on checkpoint resume).
    #[must_use]
    pub fn degraded(self) -> Self {
        match self {
            Self::Full => Self::SummaryHigh,
            other => other,
        }
    }
}

impl fmt::Display for Fidelity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Full => "full",
            Self::Truncate => "truncate",
            Self::Compact => "compact",
            Self::SummaryLow => "summary:low",
            Self::SummaryMedium => "summary:medium",
            Self::SummaryHigh => "summary:high",
        };
        write!(f, "{s}")
    }
}

impl FromStr for Fidelity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full" => Ok(Self::Full),
            "truncate" => Ok(Self::Truncate),
            "compact" => Ok(Self::Compact),
            "summary:low" => Ok(Self::SummaryLow),
            "summary:medium" => Ok(Self::SummaryMedium),
            "summary:high" => Ok(Self::SummaryHigh),
            other => Err(format!("unknown fidelity mode: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fidelity_display_roundtrips() {
        let modes = [
            Fidelity::Full,
            Fidelity::Truncate,
            Fidelity::Compact,
            Fidelity::SummaryLow,
            Fidelity::SummaryMedium,
            Fidelity::SummaryHigh,
        ];
        for mode in modes {
            let s = mode.to_string();
            let parsed: Fidelity = s.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn fidelity_default_is_compact() {
        assert_eq!(Fidelity::default(), Fidelity::Compact);
    }

    #[test]
    fn fidelity_degraded_full_becomes_summary_high() {
        assert_eq!(Fidelity::Full.degraded(), Fidelity::SummaryHigh);
    }

    #[test]
    fn fidelity_degraded_non_full_unchanged() {
        assert_eq!(Fidelity::Compact.degraded(), Fidelity::Compact);
        assert_eq!(Fidelity::SummaryHigh.degraded(), Fidelity::SummaryHigh);
    }

    #[test]
    fn fidelity_unknown_mode_errors() {
        assert!("bogus".parse::<Fidelity>().is_err());
    }
}
