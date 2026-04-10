//! Schema version handling.
//!
//! The settings schema version lives under the reserved top-level key
//! `_version`. Missing defaults to `1`. The legacy top-level `version` key is
//! a targeted rename hint. Unsupported higher versions hard-fail with an
//! upgrade hint before deeper validation continues.

use std::fmt;

/// The highest schema version this parser can consume.
pub const CURRENT_VERSION: u32 = 1;

/// An error returned when `_version` pre-validation fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// The file contains a legacy top-level `version` key. Offer a rename hint.
    LegacyVersionKey,
    /// The file declares `_version` higher than [`CURRENT_VERSION`].
    UnsupportedHigherVersion { found: u32 },
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LegacyVersionKey => f.write_str(
                "settings files must use `_version` instead of `version`. Rename the key and try again.",
            ),
            Self::UnsupportedHigherVersion { found } => write!(
                f,
                "settings schema version {found} is newer than this build supports (current: {CURRENT_VERSION}). Upgrade Fabro to read this file."
            ),
        }
    }
}

impl std::error::Error for VersionError {}

/// The parsed schema version for a settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaVersion(pub u32);

impl Default for SchemaVersion {
    fn default() -> Self {
        Self(CURRENT_VERSION)
    }
}

/// Validate and extract the schema version from a parsed TOML value before
/// deeper validation continues.
///
/// This function peeks at the top-level table and enforces three rules:
///
/// 1. `version = ...` (no underscore) is an explicit rename hint error.
/// 2. `_version` higher than [`CURRENT_VERSION`] is an upgrade hint error.
/// 3. Missing `_version` defaults to [`CURRENT_VERSION`].
pub fn validate_version(raw: &toml::Value) -> Result<SchemaVersion, VersionError> {
    if let Some(table) = raw.as_table() {
        if table.contains_key("version") {
            return Err(VersionError::LegacyVersionKey);
        }
        if let Some(value) = table.get("_version") {
            if let Some(n) = value.as_integer() {
                let found = u32::try_from(n).unwrap_or(u32::MAX);
                if found > CURRENT_VERSION {
                    return Err(VersionError::UnsupportedHigherVersion { found });
                }
                return Ok(SchemaVersion(found));
            }
        }
    }
    Ok(SchemaVersion::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> toml::Value {
        toml::from_str(input).expect("toml parse")
    }

    #[test]
    fn missing_version_defaults_to_current() {
        let raw = parse("");
        let v = validate_version(&raw).unwrap();
        assert_eq!(v, SchemaVersion(CURRENT_VERSION));
    }

    #[test]
    fn explicit_version_one_is_accepted() {
        let raw = parse("_version = 1");
        let v = validate_version(&raw).unwrap();
        assert_eq!(v, SchemaVersion(1));
    }

    #[test]
    fn legacy_version_key_errors_with_rename_hint() {
        let raw = parse("version = 1");
        let err = validate_version(&raw).unwrap_err();
        assert_eq!(err, VersionError::LegacyVersionKey);
        assert!(err.to_string().contains("_version"));
    }

    #[test]
    fn unsupported_higher_version_errors_with_upgrade_hint() {
        let raw = parse("_version = 99");
        let err = validate_version(&raw).unwrap_err();
        assert_eq!(err, VersionError::UnsupportedHigherVersion { found: 99 });
        assert!(err.to_string().contains("Upgrade"));
    }
}
