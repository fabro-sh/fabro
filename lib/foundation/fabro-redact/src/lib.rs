//! Secret and credential redaction utilities.
//!
//! `redact_string`, `redact_json_value`, and `redact_jsonl_line` provide
//! generic secret scanning: Shannon entropy over alphanumeric tokens plus
//! gitleaks credential patterns. Tokens that are ULIDs (Fabro's run, session,
//! turn, message and pair IDs) are exempt from the entropy layer, so a run ID
//! that happens to have many distinct characters is never turned into
//! `REDACTED`. [`DisplaySafeUrl`] provides deterministic URL redaction for
//! logging and error-message boundaries.

mod entropy;
mod gitleaks;
mod jsonl;
mod redactor;
mod safe_url;

pub use jsonl::{redact_json_value, redact_jsonl_line};
pub use redactor::SecretRedactor;
pub use safe_url::{DisplaySafeUrl, DisplaySafeUrlError};

pub(crate) const REDACTION_MARKER: &str = "REDACTED";

/// Redact a URL string for log or error output.
///
/// Returns the credential-redacted form when `url` parses as a URL, or a fixed
/// `"<invalid url>"` placeholder when it does not. This is the one place log
/// sites should reach for instead of re-rolling the
/// [`DisplaySafeUrl::parse`] + [`DisplaySafeUrl::redacted_string`] fallback
/// themselves, so an unparseable or credential-bearing URL never leaks into a
/// log line.
#[must_use]
pub fn redacted_url_for_log(url: &str) -> String {
    DisplaySafeUrl::parse(url)
        .map_or_else(|_| "<invalid url>".to_string(), |url| url.redacted_string())
}

/// A byte range within a string that should be redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start: usize,
    pub end:   usize,
}

/// Replace all detected secrets in `s` with "REDACTED".
///
/// Uses two layers of detection:
/// 1. Shannon entropy on high-entropy alphanumeric tokens
/// 2. Gitleaks regex pattern matching (200+ known secret formats)
pub fn redact_string(s: &str) -> String {
    let mut regions = entropy::find_entropy_regions(s);
    regions.extend(gitleaks::find_gitleaks_regions(s));
    redact_regions(s, regions)
}

/// [`redact_string`], after every non-empty `value` is replaced outright.
/// For the credentials a caller handed to a process: known secrets are
/// removed whole before the scanner, so a partial scanner match cannot leave
/// a fragment of one behind.
pub fn redact_string_with_values(s: &str, values: &[String]) -> String {
    let mut text = std::borrow::Cow::Borrowed(s);
    for value in values.iter().filter(|value| !value.is_empty()) {
        if text.contains(value.as_str()) {
            text = std::borrow::Cow::Owned(text.replace(value, REDACTION_MARKER));
        }
    }
    redact_string(&text)
}

/// Replace each region of `s` with [`REDACTION_MARKER`].
///
/// Regions may be unsorted and overlapping; they are sorted by start and
/// overlapping regions are merged so the union is redacted as a single marker.
pub(crate) fn redact_regions(s: &str, mut regions: Vec<Region>) -> String {
    if regions.is_empty() {
        return s.to_string();
    }

    regions.sort_by_key(|r| r.start);

    // Merge overlapping regions
    let mut merged = vec![regions[0].clone()];
    for r in &regions[1..] {
        let last = merged
            .last_mut()
            .expect("merged regions should be non-empty during overlap coalescing");
        if r.start <= last.end {
            last.end = last.end.max(r.end);
        } else {
            merged.push(r.clone());
        }
    }

    let mut result = String::with_capacity(s.len());
    let mut prev = 0;
    for r in &merged {
        result.push_str(&s[prev..r.start]);
        result.push_str(REDACTION_MARKER);
        prev = r.end;
    }
    result.push_str(&s[prev..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const HIGH_ENTROPY_SECRET: &str = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";

    #[test]
    fn redact_string_no_secrets() {
        assert_eq!(redact_string("hello world"), "hello world");
    }

    #[test]
    fn redacted_url_for_log_redacts_credentials() {
        assert_eq!(
            redacted_url_for_log("https://user:secret@example.com/hook?token=literal&keep=value"),
            "https://user:****@example.com/hook?token=****&keep=value"
        );
    }

    #[test]
    fn redacted_url_for_log_uses_placeholder_when_unparseable() {
        assert_eq!(redacted_url_for_log("{{ env.HOOK_URL }}"), "<invalid url>");
    }

    #[test]
    fn redact_string_with_aws_key() {
        let result = redact_string("key=AKIAYRWQG5EJLPZLBYNP");
        assert_eq!(result, "key=REDACTED");
    }

    #[test]
    fn redact_string_keeps_assignment_with_low_entropy_value() {
        // A pinned git SHA is pure hex, so the value alone can never exceed
        // 4.0 bits of entropy. Only the merged NAME=value token crosses the
        // 4.5-bit threshold, because the uppercase name widens the charset.
        // Measuring the name together with the value redacts innocuous
        // pins; the pair must survive.
        let input = "ARG DOCKER_INSTALL_COMMIT=5ce20f2eef3615d08fea941eda5a109e949e8ebf";
        assert_eq!(redact_string(input), input);
    }

    #[test]
    fn redact_string_keeps_assignment_key_for_high_entropy_value() {
        // The value alone is above the entropy threshold, so it is
        // redacted either way — but the name says which setting was
        // redacted and must survive, as the gitleaks layer already
        // does for `key=REDACTED`.
        let result = redact_string("BUILD_STAMP=xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6p");
        assert_eq!(result, "BUILD_STAMP=REDACTED");
    }

    #[test]
    fn redact_string_overlapping_detections_produce_single_redacted() {
        // A high-entropy string that also matches a gitleaks pattern
        // should produce one REDACTED, not two
        let input = format!("key={HIGH_ENTROPY_SECRET}");
        let result = redact_string(&input);
        assert_eq!(
            result.matches("REDACTED").count(),
            1,
            "expected single REDACTED, got: {result}"
        );
    }

    #[test]
    fn redact_string_two_secrets_separated_by_space() {
        let input = "key=AKIAYRWQG5EJLPZLBYNP AKIAYRWQG5EJLPZLBYNP";
        let result = redact_string(input);
        assert_eq!(result, "key=REDACTED REDACTED");
    }

    #[test]
    fn redact_string_file_path_preserved() {
        let input = "/tmp/test/controller.go";
        assert_eq!(redact_string(input), input);
    }

    #[test]
    fn redact_string_json_escape_preserved() {
        let input = r"controller.go\nmodel.go";
        assert_eq!(redact_string(input), input);
    }

    #[test]
    fn redact_string_github_pat() {
        let input = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123";
        let result = redact_string(input);
        assert!(
            result.contains("REDACTED"),
            "expected REDACTED in: {result}"
        );
    }

    #[test]
    fn redact_string_private_key() {
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        let result = redact_string(input);
        assert!(
            result.contains("REDACTED"),
            "expected REDACTED in: {result}"
        );
    }
}
