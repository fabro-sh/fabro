//! Config-facing byte sizes.
//!
//! Accepts bare integers plus `B`, `KB`, `MB`, `GB`, `TB`, and `KiB`, `MiB`,
//! `GiB`, `TiB`. Decimal units (`KB`, …) are powers of 1000; binary units
//! (`KiB`, …) are powers of 1024. Bare values default to `GB`. Fractional
//! values are not supported in the first pass. The canonical renderer emits
//! the largest decimal unit that represents the value as an integer multiple.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A byte size parsed from a human-readable string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Size(u64);

impl Size {
    #[must_use]
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn from_gigabytes(gb: u64) -> Self {
        Self(gb.saturating_mul(1_000_000_000))
    }
}

/// An error returned when parsing a size string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseSizeError {
    /// The input was empty or whitespace only.
    Empty,
    /// The input had a numeric portion that could not be parsed as a non-negative integer.
    InvalidNumber { input: String },
    /// The input contained a fractional value, which is not supported in the first pass.
    Fractional { input: String },
    /// The input had an unrecognized unit suffix.
    InvalidUnit { input: String, unit: String },
    /// The resulting value overflowed a `u64`.
    Overflow { input: String },
}

impl fmt::Display for ParseSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("size value is empty"),
            Self::InvalidNumber { input } => {
                write!(
                    f,
                    "size {input:?}: numeric portion is not a non-negative integer"
                )
            }
            Self::Fractional { input } => {
                write!(
                    f,
                    "size {input:?}: fractional values are not supported in the first pass"
                )
            }
            Self::InvalidUnit { input, unit } => {
                write!(
                    f,
                    "size {input:?}: unknown unit {unit:?} (expected one of B, KB, MB, GB, TB, KiB, MiB, GiB, TiB)"
                )
            }
            Self::Overflow { input } => {
                write!(f, "size {input:?}: value overflows u64 bytes")
            }
        }
    }
}

impl std::error::Error for ParseSizeError {}

impl FromStr for Size {
    type Err = ParseSizeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ParseSizeError::Empty);
        }

        if trimmed.contains('.') {
            return Err(ParseSizeError::Fractional {
                input: input.to_owned(),
            });
        }

        let first_alpha = trimmed.find(|c: char| !c.is_ascii_digit());
        let (num, unit) = match first_alpha {
            Some(0) => {
                return Err(ParseSizeError::InvalidNumber {
                    input: input.to_owned(),
                });
            }
            Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
            None => (trimmed, ""),
        };

        let n: u64 = num.parse().map_err(|_| ParseSizeError::InvalidNumber {
            input: input.to_owned(),
        })?;

        // Bare integers default to GB per R84.
        let multiplier: u64 = match unit {
            "" | "GB" => 1_000_000_000,
            "B" => 1,
            "KB" => 1_000,
            "MB" => 1_000_000,
            "TB" => 1_000_000_000_000,
            "KiB" => 1_024,
            "MiB" => 1_024 * 1_024,
            "GiB" => 1_024 * 1_024 * 1_024,
            "TiB" => 1_024 * 1_024 * 1_024 * 1_024,
            other => {
                return Err(ParseSizeError::InvalidUnit {
                    input: input.to_owned(),
                    unit: other.to_owned(),
                });
            }
        };

        let bytes = n
            .checked_mul(multiplier)
            .ok_or_else(|| ParseSizeError::Overflow {
                input: input.to_owned(),
            })?;

        Ok(Self(bytes))
    }
}

impl fmt::Display for Size {
    /// Canonical rendering picks the largest decimal unit that represents the
    /// value as an integer multiple, preferring `TB`, `GB`, `MB`, `KB`, `B`.
    /// Zero renders as `0B`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const KB: u64 = 1_000;
        const MB: u64 = 1_000_000;
        const GB: u64 = 1_000_000_000;
        const TB: u64 = 1_000_000_000_000;

        let b = self.0;
        if b == 0 {
            return f.write_str("0B");
        }
        if b.is_multiple_of(TB) {
            write!(f, "{}TB", b / TB)
        } else if b.is_multiple_of(GB) {
            write!(f, "{}GB", b / GB)
        } else if b.is_multiple_of(MB) {
            write!(f, "{}MB", b / MB)
        } else if b.is_multiple_of(KB) {
            write!(f, "{}KB", b / KB)
        } else {
            write!(f, "{b}B")
        }
    }
}

impl Serialize for Size {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SizeVisitor;

        impl Visitor<'_> for SizeVisitor {
            type Value = Size;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#"a size string such as "8GB" or "512MiB", or a bare integer interpreted as GB"#)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Size, E> {
                value.parse().map_err(de::Error::custom)
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Size, E> {
                self.visit_str(&value)
            }

            // Bare integers (non-negative) should parse as GB.
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Size, E> {
                Ok(Size::from_gigabytes(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Size, E> {
                u64::try_from(value)
                    .map(Size::from_gigabytes)
                    .map_err(|_| de::Error::custom("size must be non-negative"))
            }
        }

        deserializer.deserialize_any(SizeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_integer_defaults_to_gb() {
        assert_eq!("8".parse::<Size>().unwrap(), Size::from_gigabytes(8));
    }

    #[test]
    fn parses_decimal_units() {
        assert_eq!("1B".parse::<Size>().unwrap().as_bytes(), 1);
        assert_eq!("1KB".parse::<Size>().unwrap().as_bytes(), 1_000);
        assert_eq!("1MB".parse::<Size>().unwrap().as_bytes(), 1_000_000);
        assert_eq!("1GB".parse::<Size>().unwrap().as_bytes(), 1_000_000_000);
        assert_eq!("1TB".parse::<Size>().unwrap().as_bytes(), 1_000_000_000_000);
    }

    #[test]
    fn parses_binary_units() {
        assert_eq!("1KiB".parse::<Size>().unwrap().as_bytes(), 1_024);
        assert_eq!("1MiB".parse::<Size>().unwrap().as_bytes(), 1_024 * 1_024);
        assert_eq!(
            "1GiB".parse::<Size>().unwrap().as_bytes(),
            1_024 * 1_024 * 1_024
        );
        assert_eq!(
            "1TiB".parse::<Size>().unwrap().as_bytes(),
            1_024u64 * 1_024 * 1_024 * 1_024
        );
    }

    #[test]
    fn rejects_fractional_values() {
        let err = "1.5GB".parse::<Size>().unwrap_err();
        assert!(matches!(err, ParseSizeError::Fractional { .. }));
    }

    #[test]
    fn rejects_unknown_units() {
        let err = "5XB".parse::<Size>().unwrap_err();
        assert!(matches!(err, ParseSizeError::InvalidUnit { unit, .. } if unit == "XB"));
    }

    #[test]
    fn rejects_empty_input() {
        let err = "".parse::<Size>().unwrap_err();
        assert!(matches!(err, ParseSizeError::Empty));
    }

    #[test]
    fn canonical_render_uses_largest_decimal_unit() {
        assert_eq!(Size::from_bytes(1_000_000_000_000).to_string(), "1TB");
        assert_eq!(Size::from_bytes(2_000_000_000).to_string(), "2GB");
        assert_eq!(Size::from_bytes(5_000_000).to_string(), "5MB");
        assert_eq!(Size::from_bytes(3_000).to_string(), "3KB");
        assert_eq!(Size::from_bytes(0).to_string(), "0B");
    }

    #[test]
    fn canonical_render_falls_back_to_bytes_for_odd_values() {
        // 1_500 bytes is not a whole MB/KB. 1_500 / 1_000 = 1.5 → falls back to B.
        assert_eq!(Size::from_bytes(1_500).to_string(), "1500B");
    }

    #[test]
    fn binary_unit_values_render_as_bytes_when_not_a_whole_decimal_unit() {
        // 1 KiB = 1024 bytes — not divisible by 1000, so canonical render is bytes.
        let size = "1KiB".parse::<Size>().unwrap();
        assert_eq!(size.to_string(), "1024B");
    }

    #[test]
    fn overflow_detected() {
        let err = format!("{}TB", u64::MAX).parse::<Size>().unwrap_err();
        assert!(matches!(err, ParseSizeError::Overflow { .. }));
    }

    #[test]
    fn serde_round_trip_via_json_string() {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct Wrap {
            s: Size,
        }

        let input = r#"{"s":"8GB"}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert_eq!(parsed.s, Size::from_gigabytes(8));
        let rendered = serde_json::to_string(&parsed).unwrap();
        assert_eq!(rendered, r#"{"s":"8GB"}"#);
    }

    #[test]
    fn serde_accepts_bare_integer_as_gb() {
        #[derive(Debug, serde::Deserialize, PartialEq)]
        struct Wrap {
            s: Size,
        }

        let input = r#"{"s":8}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert_eq!(parsed.s, Size::from_gigabytes(8));
    }
}
