#![allow(
    clippy::disallowed_types,
    reason = "fabro-redact owns the raw URL wrapper and redaction boundary"
)]

//! Credential-redacting URL display helpers.
//!
//! `DisplaySafeUrl` is a transparent wrapper around [`url::Url`] for log and
//! error-message boundaries. It keeps the raw URL accessible for network,
//! shell, and persistence code, while making [`Display`] and [`Debug`] render a
//! redacted form by default.
//!
//! Diverges from uv:
//! - [`Debug`] delegates to [`Display`] so `tracing::debug!(?url)` is safe.
//! - Sensitive query-string keys are redacted in addition to userinfo.
//! - The type intentionally does not implement serde; callers must choose raw
//!   or redacted output explicitly.

use std::borrow::Cow;
use std::fmt::{self, Debug, Display};
use std::ops::{Deref, DerefMut};
use std::str::FromStr;

use ref_cast::RefCast;
use thiserror::Error;
use url::{Url, form_urlencoded};

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DisplaySafeUrlError {
    /// Failed to parse a URL.
    #[error(transparent)]
    Url(#[from] url::ParseError),

    /// The URL parsed, but the apparent authority contains ambiguous
    /// unescaped credential characters.
    #[error("ambiguous user/pass authority in URL (not percent-encoded?): {0}")]
    AmbiguousAuthority(String),
}

/// A [`Url`] wrapper that redacts credentials when displayed or debug-printed.
///
/// Use [`Self::redacted_string`] for log/output text and [`Self::raw_string`]
/// or [`Self::as_raw_url`] when the real URL must go to the wire.
#[derive(Clone, Eq, PartialEq, PartialOrd, Ord, Hash, RefCast)]
#[repr(transparent)]
pub struct DisplaySafeUrl(Url);

impl DisplaySafeUrl {
    /// Parse user-provided URL text; [`FromStr`] delegates here.
    #[inline]
    pub fn parse(input: &str) -> Result<Self, DisplaySafeUrlError> {
        let url = Url::parse(input)?;
        Self::reject_ambiguous_credentials(input, &url)?;
        Ok(Self(url))
    }

    /// Cast a `&Url` to a `&DisplaySafeUrl` without allocation.
    #[inline]
    pub fn ref_cast(url: &Url) -> &Self {
        RefCast::ref_cast(url)
    }

    /// Parse a string as a URL relative to this URL.
    #[inline]
    pub fn join(&self, input: &str) -> Result<Self, DisplaySafeUrlError> {
        Ok(Self(self.0.join(input)?))
    }

    #[expect(clippy::result_unit_err, reason = "matches url::Url::from_file_path")]
    pub fn from_file_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self, ()> {
        Ok(Self(Url::from_file_path(path)?))
    }

    /// Return the raw inner URL.
    #[inline]
    pub fn as_raw_url(&self) -> &Url {
        &self.0
    }

    /// Return the redacted display form.
    #[inline]
    pub fn redacted_string(&self) -> String {
        format!("{self}")
    }

    /// Return the raw URL string.
    #[inline]
    pub fn raw_string(&self) -> String {
        self.0.to_string()
    }

    /// Remove credentials from this URL, preserving the SSH `git` username.
    #[inline]
    pub fn remove_credentials(&mut self) {
        if is_ssh_git_username(&self.0) {
            return;
        }
        let _ = self.0.set_username("");
        let _ = self.0.set_password(None);
    }

    /// Return this URL with credentials removed, preserving the SSH `git`
    /// username.
    pub fn without_credentials(&self) -> Cow<'_, Url> {
        if self.0.password().is_none() && self.0.username().is_empty() {
            return Cow::Borrowed(&self.0);
        }

        if is_ssh_git_username(&self.0) {
            return Cow::Borrowed(&self.0);
        }

        let mut url = self.0.clone();
        let _ = url.set_username("");
        let _ = url.set_password(None);
        Cow::Owned(url)
    }

    /// Return a display adapter for the raw URL.
    #[inline]
    pub fn displayable_with_credentials(&self) -> impl Display + '_ {
        &self.0
    }

    fn reject_ambiguous_credentials(input: &str, url: &Url) -> Result<(), DisplaySafeUrlError> {
        if url.scheme() == "file" || url.password().is_some() {
            return Ok(());
        }

        let has_ambiguous_path = has_unqualified_credential_like_pattern(url.path());
        let has_ambiguous_fragment = url
            .fragment()
            .is_some_and(has_unqualified_credential_like_pattern);

        if !has_ambiguous_path && !has_ambiguous_fragment {
            return Ok(());
        }

        let (Some(col_pos), Some(at_pos)) = (input.find(':'), input.rfind('@')) else {
            if cfg!(debug_assertions) {
                unreachable!(
                    "`:` or `@` sign missing in URL that was confirmed to contain them: {input}"
                );
            }
            return Ok(());
        };

        let redacted_path = format!("{}***{}", &input[0..=col_pos], &input[at_pos..]);
        Err(DisplaySafeUrlError::AmbiguousAuthority(redacted_path))
    }
}

impl Deref for DisplaySafeUrl {
    type Target = Url;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DisplaySafeUrl {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Display for DisplaySafeUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        display_redacted_url(&self.0, formatter)
    }
}

impl Debug for DisplaySafeUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl From<DisplaySafeUrl> for Url {
    fn from(url: DisplaySafeUrl) -> Self {
        url.0
    }
}

impl From<Url> for DisplaySafeUrl {
    fn from(url: Url) -> Self {
        Self(url)
    }
}

impl FromStr for DisplaySafeUrl {
    type Err = DisplaySafeUrlError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

fn display_redacted_url(url: &Url, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    if url.cannot_be_a_base() {
        return write!(formatter, "{url}");
    }

    write!(formatter, "{}://", url.scheme())?;

    if is_ssh_git_username(url) {
        write!(formatter, "{}@", url.username())?;
    } else if !url.username().is_empty() && url.password().is_some() {
        write!(formatter, "{}:****@", url.username())?;
    } else if !url.username().is_empty() {
        write!(formatter, "{}@", url.username())?;
    } else if url.password().is_some() {
        write!(formatter, ":****@")?;
    }

    write!(formatter, "{}", url.host_str().unwrap_or(""))?;

    if let Some(port) = url.port() {
        write!(formatter, ":{port}")?;
    }

    write!(formatter, "{}", redact_credential_like_patterns(url.path()))?;

    if let Some(query) = redacted_query(url) {
        write!(formatter, "?{query}")?;
    }

    if let Some(fragment) = url.fragment() {
        write!(formatter, "#{}", redact_credential_like_patterns(fragment))?;
    }

    Ok(())
}

fn redacted_query(url: &Url) -> Option<String> {
    url.query().and_then(|query| {
        if query.is_empty() {
            return None;
        }

        let mut serializer = form_urlencoded::Serializer::new(String::new());
        for (key, value) in url.query_pairs() {
            let value = if is_sensitive_query_key(&key) {
                Cow::Borrowed("****")
            } else {
                value
            };
            serializer.append_pair(&key, &value);
        }

        Some(serializer.finish())
    })
}

fn is_sensitive_query_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "token"
            | "install_token"
            | "access_token"
            | "refresh_token"
            | "api_key"
            | "apikey"
            | "code"
            | "state"
            | "password"
            | "secret"
            | "key"
    )
}

fn is_ssh_git_username(url: &Url) -> bool {
    matches!(url.scheme(), "ssh" | "git+ssh" | "git+https")
        && url.username() == "git"
        && url.password().is_none()
}

fn has_unqualified_credential_like_pattern(input: &str) -> bool {
    let mut search_start = 0;
    while let Some(relative_colon) = input[search_start..].find(':') {
        let colon = search_start + relative_colon;
        let after_colon = &input[colon + 1..];
        if after_colon.starts_with("//") {
            search_start = colon + 3;
            continue;
        }

        if after_colon.contains('@') && !input[..colon].contains("://") {
            return true;
        }

        search_start = colon + 1;
    }

    false
}

fn redact_credential_like_patterns(input: &str) -> Cow<'_, str> {
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = String::new();

    while let Some(relative_colon) = input[search_start..].find(':') {
        let colon = search_start + relative_colon;
        let after_colon = &input[colon + 1..];
        if after_colon.starts_with("//") {
            search_start = colon + 3;
            continue;
        }

        let Some(relative_at) = after_colon.find('@') else {
            break;
        };
        let at = colon + 1 + relative_at;

        output.push_str(&input[copy_start..=colon]);
        output.push_str("****@");
        copy_start = at + 1;
        search_start = at + 1;
    }

    if copy_start == 0 {
        Cow::Borrowed(input)
    } else {
        output.push_str(&input[copy_start..]);
        Cow::Owned(output)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::{fmt, io};

    use tracing::{debug, subscriber};
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::fmt::{self as tracing_fmt, MakeWriter};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry;

    use super::{DisplaySafeUrl, DisplaySafeUrlError};

    #[test]
    fn display_redacts_userinfo_password() {
        let url = DisplaySafeUrl::parse("https://user:secret@example.com").unwrap();

        assert_eq!(url.redacted_string(), "https://user:****@example.com/");
        assert_eq!(url.to_string(), url.redacted_string());
    }

    #[test]
    fn display_leaves_plain_url_unchanged() {
        let url = DisplaySafeUrl::parse("https://example.com/path").unwrap();

        assert_eq!(url.redacted_string(), "https://example.com/path");
        assert_eq!(url.raw_string(), "https://example.com/path");
    }

    #[test]
    fn debug_delegates_to_display() {
        let url = DisplaySafeUrl::parse("https://example.com/install?token=ghs_secret").unwrap();

        assert_eq!(format!("{url:?}"), format!("{url}"));
        assert_eq!(format!("{url:?}"), "https://example.com/install?token=****");
    }

    #[test]
    fn raw_access_keeps_credentials_available_for_wire_use() {
        let mut url = DisplaySafeUrl::parse("https://user:secret@example.com/path").unwrap();

        assert_eq!(url.username(), "user");
        assert_eq!(url.password(), Some("secret"));
        assert_eq!(url.raw_string(), "https://user:secret@example.com/path");
        assert_eq!(
            url.as_raw_url().as_str(),
            "https://user:secret@example.com/path"
        );

        url.set_username("other").unwrap();
        url.set_password(Some("new-secret")).unwrap();
        assert_eq!(
            url.raw_string(),
            "https://other:new-secret@example.com/path"
        );

        url.remove_credentials();
        assert_eq!(url.raw_string(), "https://example.com/path");
    }

    #[test]
    fn display_redacts_sensitive_query_keys_case_insensitively() {
        let url = DisplaySafeUrl::parse("https://example.com/cb?code=X&state=Y&keep=Z&API_KEY=abc")
            .unwrap();

        assert_eq!(
            url.redacted_string(),
            "https://example.com/cb?code=****&state=****&keep=Z&API_KEY=****"
        );
    }

    #[test]
    fn display_does_not_redact_query_key_prefixes() {
        let url = DisplaySafeUrl::parse("https://example.com/cb?tokenish=abc&keyed=xyz").unwrap();

        assert_eq!(
            url.redacted_string(),
            "https://example.com/cb?tokenish=abc&keyed=xyz"
        );
    }

    #[test]
    fn display_preserves_ipv6_host_brackets() {
        let url = DisplaySafeUrl::parse("https://[::1]:8080/cb?token=abc").unwrap();

        assert_eq!(url.redacted_string(), "https://[::1]:8080/cb?token=****");
    }

    #[test]
    fn display_redacts_nested_proxy_credentials() {
        let url =
            DisplaySafeUrl::parse("git+https://proxy.com/https://user:pw@github.com/repo").unwrap();

        assert_eq!(
            url.redacted_string(),
            "git+https://proxy.com/https://user:****@github.com/repo"
        );
    }

    #[test]
    fn display_handles_urls_without_passwords() {
        let url = DisplaySafeUrl::parse("https://user@example.com/").unwrap();

        assert_eq!(url.redacted_string(), "https://user@example.com/");
    }

    #[test]
    fn display_masks_password_when_username_is_empty() {
        let url = DisplaySafeUrl::parse("https://:secret@example.com/").unwrap();

        assert_eq!(url.redacted_string(), "https://:****@example.com/");
    }

    #[test]
    fn parse_rejects_ambiguous_authority() {
        let err = DisplaySafeUrl::parse("https://user/name:pw@host").unwrap_err();

        assert!(matches!(err, DisplaySafeUrlError::AmbiguousAuthority(_)));
    }

    #[test]
    fn tracing_debug_output_redacts_token_bearing_urls() {
        let output = CapturedTrace::default();
        let subscriber = registry().with(
            tracing_fmt::layer()
                .with_writer(output.clone())
                .with_span_events(FmtSpan::NONE)
                .without_time()
                .with_target(false),
        );

        subscriber::with_default(subscriber, || {
            let url =
                DisplaySafeUrl::parse("https://example.com/install?token=ghs_secret").unwrap();
            debug!(?url, %url, "constructed install url");
        });

        let formatted = output.captured_output();
        assert!(formatted.contains("token=****"));
        assert!(!formatted.contains("ghs_secret"));
    }

    #[derive(Clone, Default)]
    struct CapturedTrace {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl CapturedTrace {
        fn captured_output(&self) -> String {
            let buffer = self.buffer.lock().unwrap();
            String::from_utf8(buffer.clone()).unwrap()
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedTrace {
        type Writer = CapturedTraceWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedTraceWriter {
                buffer: Arc::clone(&self.buffer),
            }
        }
    }

    struct CapturedTraceWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl io::Write for CapturedTraceWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl fmt::Debug for CapturedTraceWriter {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("CapturedTraceWriter").finish()
        }
    }
}
