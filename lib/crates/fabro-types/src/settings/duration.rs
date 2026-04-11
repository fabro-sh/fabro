//! Config-facing duration values.
//!
//! Accepts a single-unit suffix per value: `ms`, `s`, `m`, `h`, or `d`.
//! Composed values like `1h30m` are rejected. The canonical renderer emits
//! the same single-unit form.

use std::fmt;
use std::str::FromStr;
use std::time::Duration as StdDuration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A duration parsed from a single-unit human-readable string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration(StdDuration);

impl Duration {
    #[must_use]
    pub const fn from_std(duration: StdDuration) -> Self {
        Self(duration)
    }

    #[must_use]
    pub const fn as_std(&self) -> StdDuration {
        self.0
    }

    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(StdDuration::from_secs(secs))
    }

    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(StdDuration::from_millis(millis))
    }
}

impl From<StdDuration> for Duration {
    fn from(value: StdDuration) -> Self {
        Self(value)
    }
}

impl From<Duration> for StdDuration {
    fn from(value: Duration) -> Self {
        value.0
    }
}

/// An error returned when parsing a duration string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDurationError {
    /// The input was empty or whitespace only.
    Empty,
    /// The input had a numeric portion that could not be parsed as a
    /// non-negative integer.
    InvalidNumber { input: String },
    /// The input had no unit suffix.
    MissingUnit { input: String },
    /// The input had an unrecognized unit suffix.
    InvalidUnit { input: String, unit: String },
    /// The input contained more than one unit (e.g. `1h30m`), which is not
    /// supported.
    Composed { input: String },
}

impl fmt::Display for ParseDurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("duration value is empty"),
            Self::InvalidNumber { input } => {
                write!(
                    f,
                    "duration {input:?}: numeric portion is not a non-negative integer"
                )
            }
            Self::MissingUnit { input } => {
                write!(
                    f,
                    "duration {input:?}: missing unit suffix (expected one of ms, s, m, h, d)"
                )
            }
            Self::InvalidUnit { input, unit } => {
                write!(
                    f,
                    "duration {input:?}: unknown unit {unit:?} (expected one of ms, s, m, h, d)"
                )
            }
            Self::Composed { input } => {
                write!(
                    f,
                    "duration {input:?}: composed values like 1h30m are not supported; use the smallest needed unit instead"
                )
            }
        }
    }
}

impl std::error::Error for ParseDurationError {}

impl FromStr for Duration {
    type Err = ParseDurationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ParseDurationError::Empty);
        }

        // Split numeric prefix from alphabetic suffix. Reject interior alpha runs.
        let (num, unit) =
            split_number_and_suffix(trimmed).ok_or_else(|| ParseDurationError::MissingUnit {
                input: input.to_owned(),
            })?;

        if num.is_empty() {
            return Err(ParseDurationError::InvalidNumber {
                input: input.to_owned(),
            });
        }

        // A composed value like "1h30m" leaves digits inside the suffix.
        if unit.chars().any(|c| c.is_ascii_digit()) {
            return Err(ParseDurationError::Composed {
                input: input.to_owned(),
            });
        }

        let n: u64 = num.parse().map_err(|_| ParseDurationError::InvalidNumber {
            input: input.to_owned(),
        })?;

        let duration = match unit {
            "ms" => StdDuration::from_millis(n),
            "s" => StdDuration::from_secs(n),
            "m" => StdDuration::from_secs(n.saturating_mul(60)),
            "h" => StdDuration::from_secs(n.saturating_mul(60 * 60)),
            "d" => StdDuration::from_secs(n.saturating_mul(24 * 60 * 60)),
            other => {
                return Err(ParseDurationError::InvalidUnit {
                    input: input.to_owned(),
                    unit:  other.to_owned(),
                });
            }
        };

        Ok(Self(duration))
    }
}

/// Split a trimmed string into `(numeric_prefix, alphabetic_suffix)`.
///
/// Returns `None` if there is no alphabetic suffix or the string is all alpha.
fn split_number_and_suffix(input: &str) -> Option<(&str, &str)> {
    let first_alpha = input.find(|c: char| !c.is_ascii_digit())?;
    if first_alpha == 0 {
        return None;
    }
    Some((&input[..first_alpha], &input[first_alpha..]))
}

impl fmt::Display for Duration {
    /// Canonical rendering picks the largest unit that represents the value as
    /// an integer multiple, preferring `d`, `h`, `m`, `s`, `ms` in that
    /// order. Zero renders as `0s`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MS_PER_S: u128 = 1_000;
        const MS_PER_M: u128 = 60 * MS_PER_S;
        const MS_PER_H: u128 = 60 * MS_PER_M;
        const MS_PER_D: u128 = 24 * MS_PER_H;

        let total_ms = self.0.as_millis();
        if total_ms == 0 {
            return f.write_str("0s");
        }
        if total_ms.is_multiple_of(MS_PER_D) {
            write!(f, "{}d", total_ms / MS_PER_D)
        } else if total_ms.is_multiple_of(MS_PER_H) {
            write!(f, "{}h", total_ms / MS_PER_H)
        } else if total_ms.is_multiple_of(MS_PER_M) {
            write!(f, "{}m", total_ms / MS_PER_M)
        } else if total_ms.is_multiple_of(MS_PER_S) {
            write!(f, "{}s", total_ms / MS_PER_S)
        } else {
            write!(f, "{total_ms}ms")
        }
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DurationVisitor;

        impl Visitor<'_> for DurationVisitor {
            type Value = Duration;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#"a duration string such as "30s", "1m", or "1h""#)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Duration, E> {
                value.parse().map_err(de::Error::custom)
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Duration, E> {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_str(DurationVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_unit() {
        assert_eq!(
            "500ms".parse::<Duration>().unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!("30s".parse::<Duration>().unwrap(), Duration::from_secs(30));
        assert_eq!("2m".parse::<Duration>().unwrap(), Duration::from_secs(120));
        assert_eq!(
            "1h".parse::<Duration>().unwrap(),
            Duration::from_secs(3_600)
        );
        assert_eq!(
            "1d".parse::<Duration>().unwrap(),
            Duration::from_secs(86_400)
        );
    }

    #[test]
    fn rejects_composed_values() {
        let err = "1h30m".parse::<Duration>().unwrap_err();
        assert!(matches!(err, ParseDurationError::Composed { .. }));
    }

    #[test]
    fn rejects_missing_unit() {
        let err = "30".parse::<Duration>().unwrap_err();
        assert!(matches!(err, ParseDurationError::MissingUnit { .. }));
    }

    #[test]
    fn rejects_unknown_unit() {
        let err = "1w".parse::<Duration>().unwrap_err();
        assert!(matches!(err, ParseDurationError::InvalidUnit { unit, .. } if unit == "w"));
    }

    #[test]
    fn rejects_empty() {
        let err = "".parse::<Duration>().unwrap_err();
        assert!(matches!(err, ParseDurationError::Empty));
    }

    #[test]
    fn canonical_render_picks_largest_unit() {
        assert_eq!(Duration::from_secs(86_400).to_string(), "1d");
        assert_eq!(Duration::from_secs(3_600).to_string(), "1h");
        assert_eq!(Duration::from_secs(120).to_string(), "2m");
        assert_eq!(Duration::from_secs(30).to_string(), "30s");
        assert_eq!(Duration::from_millis(500).to_string(), "500ms");
        assert_eq!(Duration::from_millis(0).to_string(), "0s");
    }

    #[test]
    fn canonical_render_rounds_down_units_that_do_not_divide() {
        // 90s cannot render as a whole number of minutes.
        assert_eq!(Duration::from_secs(90).to_string(), "90s");
    }

    #[test]
    fn round_trip_through_parse_and_display() {
        for input in ["500ms", "30s", "1m", "2h", "3d"] {
            let parsed: Duration = input.parse().unwrap();
            assert_eq!(parsed.to_string(), input);
        }
    }

    #[test]
    fn serde_round_trip_via_json() {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct Wrap {
            d: Duration,
        }

        let input = r#"{"d":"30s"}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert_eq!(parsed.d, Duration::from_secs(30));
        let rendered = serde_json::to_string(&parsed).unwrap();
        assert_eq!(rendered, r#"{"d":"30s"}"#);
    }
}
