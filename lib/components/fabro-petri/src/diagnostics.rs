//! Bounded, safe command diagnostics, kept separate from error summaries.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use fabro_types::{ExecOutputTail, FailureCategory, FailureDetail};
use fabro_util::error;
use pebble_coding_agent::char_boundary::ceil_char_boundary;
use pebble_coding_agent::environment::{DEFAULT_EXEC_OUTPUT_TAIL_BYTES, ExecResult};
use pebble_coding_agent::events::CommandTermination;
use pebble_coding_agent::extensions::Redactor;

use crate::checkpoint::CheckpointError;
use crate::source::SourceCredential;

/// Sanitized output for a failed command. Debug deliberately reports metadata
/// only: attaching this to an error must not dump command output into tracing.
#[derive(Clone)]
pub struct CommandOutput(Option<ExecOutputTail>);

impl fmt::Debug for CommandOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        ExecOutputTail::trace_summary(self.0.as_ref()).fmt(f)
    }
}

impl CommandOutput {
    /// The bounded tail of a command's streams, with `credential` (the one
    /// the command was handed) and recognizable secrets masked. The tail
    /// starts on a whole line where one fits.
    pub(crate) fn capture(
        stdout: &[u8],
        stderr: &[u8],
        credential: Option<&SourceCredential>,
    ) -> Self {
        let redactor = ValueRedactor(credential.map(SourceCredential::sensitive_values));
        let result = ExecResult {
            stdout:      String::from_utf8_lossy(stdout).into_owned(),
            stderr:      String::from_utf8_lossy(stderr).into_owned(),
            exit_code:   None,
            termination: CommandTermination::Exited,
            duration_ms: 0,
        };
        // One byte over the limit through the shared sanitizer, so a tail
        // that happens to start at a newline keeps its whole first line.
        let mut tail = result.output_tail(&redactor, DEFAULT_EXEC_OUTPUT_TAIL_BYTES + 1);
        if let Some(tail) = &mut tail {
            for (text, truncated) in [
                (&mut tail.stdout, &mut tail.stdout_truncated),
                (&mut tail.stderr, &mut tail.stderr_truncated),
            ] {
                let Some(text) = text else {
                    continue;
                };
                *truncated |= text.len() > DEFAULT_EXEC_OUTPUT_TAIL_BYTES;
                if *truncated {
                    text.drain(..line_start(text, DEFAULT_EXEC_OUTPUT_TAIL_BYTES));
                }
            }
        }
        Self(tail)
    }
}

/// Where a tail of at most `limit` bytes of `text` begins: at the first
/// whole line within the limit, or mid-line when the limit holds no whole
/// line.
fn line_start(text: &str, limit: usize) -> usize {
    let start = ceil_char_boundary(text, text.len().saturating_sub(limit));
    if start == 0 || text.as_bytes()[start - 1] == b'\n' {
        return start;
    }
    text[start..]
        .find('\n')
        .map(|newline| start + newline + 1)
        .filter(|next| *next < text.len())
        .unwrap_or(start)
}

/// The source chain remains typed until this projection boundary. Only command
/// failures currently contribute output; other errors keep their existing path.
pub(crate) fn failure_detail(error: &(dyn Error + 'static)) -> Option<FailureDetail> {
    let mut sources = std::iter::successors(Some(error), |error| Error::source(*error));
    let output = sources.find_map(|error| match error.downcast_ref()? {
        CheckpointError::Command { output, .. } => Some(output),
        _ => None,
    })?;
    let mut chain = error::collect_chain(error).into_iter();
    let mut detail = FailureDetail::new(chain.next()?, FailureCategory::Deterministic);
    detail.causes = chain.collect();
    detail.exec_output_tail.clone_from(&output.0);
    Some(detail)
}

/// Known credentials are removed whole before the scanner and before
/// bounding, so a cut through one cannot defeat redaction.
struct ValueRedactor(Option<Vec<String>>);

impl Redactor for ValueRedactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let redacted =
            fabro_redact::redact_string_with_values(text, self.0.as_deref().unwrap_or(&[]));
        if redacted == text {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(redacted)
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    #[test]
    fn command_diagnostics_keep_the_rejection_header_and_hide_credentials() {
        let token = "an-unrecognizable-test-password";
        let encoded = STANDARD.encode(format!("x-access-token:{token}"));
        let credential = SourceCredential::from_encoded(&encoded).unwrap();
        let stderr = format!(
            "\x1b[31mremote: error: GH013: Repository rule violations found.\x1b[0m\n{}\nCannot create ref\n{token}\n{encoded}\n",
            "padding\n".repeat(100)
        );
        let output = CommandOutput::capture(b"some stdout", stderr.as_bytes(), Some(&credential));
        let tail = output.0.as_ref().unwrap();
        let text = tail.stderr.as_ref().unwrap();
        assert!(text.starts_with("remote: error: GH013:"));
        assert!(text.contains("Cannot create ref"));
        assert!(!text.contains(token));
        assert!(!text.contains(&encoded));
        assert!(!text.contains('\x1b'));
        assert!(!tail.stderr_truncated);
        assert_eq!(tail.stdout.as_deref(), Some("some stdout"));
        assert!(!format!("{output:?}").contains("GH013"));
    }

    #[test]
    fn tails_are_bounded_readable_and_report_omitted_output() {
        let text = format!("{}\nremote: useful last line\n", "x".repeat(10_000));
        let tail = CommandOutput::capture(b"", text.as_bytes(), None)
            .0
            .unwrap();
        assert_eq!(tail.stderr.as_deref(), Some("remote: useful last line\n"));
        assert!(tail.stderr_truncated);
        let exact_line = format!(
            "{}\n{}",
            "x".repeat(100),
            "y".repeat(DEFAULT_EXEC_OUTPUT_TAIL_BYTES)
        );
        let tail = CommandOutput::capture(b"", exact_line.as_bytes(), None)
            .0
            .unwrap();
        assert_eq!(
            tail.stderr.as_ref().unwrap().len(),
            DEFAULT_EXEC_OUTPUT_TAIL_BYTES
        );
        let unicode = "🦀".repeat(3000);
        let tail = CommandOutput::capture(unicode.as_bytes(), b"\xfffailure", None)
            .0
            .unwrap();
        assert!(tail.stdout.as_ref().unwrap().len() <= DEFAULT_EXEC_OUTPUT_TAIL_BYTES);
        assert!(tail.stdout_truncated);
        assert_eq!(tail.stderr.as_deref(), Some("�failure"));
        assert!(CommandOutput::capture(b"", b"", None).0.is_none());
    }

    #[test]
    fn contextual_errors_preserve_the_command_source_and_output() {
        let error = anyhow::Error::new(CheckpointError::Command {
            action: "push".into(),
            status: "non-zero exit".into(),
            output: CommandOutput::capture(b"", b"remote: rejected", None),
        })
        .context("could not publish the run");
        let detail = failure_detail(error.as_ref()).unwrap();
        assert_eq!(detail.causes, ["git push failed (non-zero exit)"]);
        assert_eq!(
            detail.exec_output_tail.unwrap().stderr.as_deref(),
            Some("remote: rejected")
        );
        assert!(!error.to_string().contains("remote:"));
    }
}
