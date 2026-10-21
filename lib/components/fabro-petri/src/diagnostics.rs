//! Bounded, safe command diagnostics, kept separate from error summaries.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fabro_redact::SecretRedactor;
use fabro_types::{ExecOutputTail, FailureCategory, FailureDetail};
use fabro_util::error;
use pebble_coding_agent::environment::{DEFAULT_EXEC_OUTPUT_TAIL_BYTES, ExecResult};
use pebble_coding_agent::events::CommandTermination;
use pebble_coding_agent::extensions::Redactor;

use crate::checkpoint::CheckpointError;

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
    pub(crate) fn capture(stdout: &[u8], stderr: &[u8], env: &[(String, String)]) -> Self {
        let result = ExecResult {
            stdout:      String::from_utf8_lossy(stdout).into_owned(),
            stderr:      String::from_utf8_lossy(stderr).into_owned(),
            exit_code:   None,
            termination: CommandTermination::Exited,
            duration_ms: 0,
        };
        let mut tail =
            result.output_tail(&GitRedactor::new(env), DEFAULT_EXEC_OUTPUT_TAIL_BYTES + 1);
        if let Some(tail) = &mut tail {
            for (text, truncated) in [
                (&mut tail.stdout, &mut tail.stdout_truncated),
                (&mut tail.stderr, &mut tail.stderr_truncated),
            ] {
                let Some(text) = text else {
                    continue;
                };
                *truncated |= text.len() > DEFAULT_EXEC_OUTPUT_TAIL_BYTES;
                if !*truncated {
                    continue;
                }
                // Capture one extra byte through the shared sanitizer so an
                // exact newline boundary does not discard a whole first line.
                let mut start = text.len().saturating_sub(DEFAULT_EXEC_OUTPUT_TAIL_BYTES);
                while !text.is_char_boundary(start) {
                    start += 1;
                }
                let whole_line = start > 0 && text.as_bytes()[start - 1] == b'\n';
                if !whole_line {
                    if let Some(newline) = text[start..].find('\n') {
                        if start + newline + 1 < text.len() {
                            start += newline + 1;
                        }
                    }
                }
                text.drain(..start);
            }
        }
        Self(tail)
    }
}

/// The source chain remains typed until this projection boundary. Only command
/// failures currently contribute output; other errors keep their existing path.
pub(crate) fn failure_detail(error: &(dyn Error + 'static)) -> Option<FailureDetail> {
    let causes = error::collect_chain(error);
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(CheckpointError::Command { output, .. }) = error.downcast_ref() {
            let mut detail = FailureDetail::new(causes[0].clone(), FailureCategory::Deterministic);
            detail.causes = causes.into_iter().skip(1).collect();
            detail.exec_output_tail.clone_from(&output.0);
            return Some(detail);
        }
        current = error.source();
    }
    None
}

// Explicit credentials must be removed before bounding: a cut through a token
// must not defeat redaction. The scanner additionally masks recognizable
// secrets.
struct GitRedactor {
    secrets: Vec<String>,
}

impl GitRedactor {
    fn new(env: &[(String, String)]) -> Self {
        let mut secrets = Vec::new();
        for (key, value) in env {
            if !key.starts_with("GIT_CONFIG_VALUE_") {
                continue;
            }
            let Some(encoded) = value.strip_prefix("AUTHORIZATION: basic ") else {
                continue;
            };
            if encoded.is_empty() {
                continue;
            }
            secrets.push(encoded.to_string());
            let Ok(decoded) = STANDARD.decode(encoded) else {
                continue;
            };
            let Ok(decoded) = String::from_utf8(decoded) else {
                continue;
            };
            if let Some((_, password)) = decoded.split_once(':') {
                if !password.is_empty() {
                    secrets.push(password.to_string());
                }
            }
        }
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        Self { secrets }
    }
}

impl Redactor for GitRedactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut text = text.to_string();
        for secret in &self.secrets {
            text = text.replace(secret, "[REDACTED]");
        }
        Cow::Owned(SecretRedactor.redact(&text).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_diagnostics_keep_the_rejection_header_and_hide_credentials() {
        let token = "an-unrecognizable-test-password";
        let encoded = STANDARD.encode(format!("x-access-token:{token}"));
        let env = vec![(
            "GIT_CONFIG_VALUE_0".into(),
            format!("AUTHORIZATION: basic {encoded}"),
        )];
        let stderr = format!(
            "\x1b[31mremote: error: GH013: Repository rule violations found.\x1b[0m\n{}\nCannot create ref\n{token}\n{encoded}\n",
            "padding\n".repeat(100)
        );
        let output = CommandOutput::capture(b"some stdout", stderr.as_bytes(), &env);
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
        let tail = CommandOutput::capture(b"", text.as_bytes(), &[]).0.unwrap();
        assert_eq!(tail.stderr.as_deref(), Some("remote: useful last line\n"));
        assert!(tail.stderr_truncated);
        let exact_line = format!(
            "{}\n{}",
            "x".repeat(100),
            "y".repeat(DEFAULT_EXEC_OUTPUT_TAIL_BYTES)
        );
        let tail = CommandOutput::capture(b"", exact_line.as_bytes(), &[])
            .0
            .unwrap();
        assert_eq!(
            tail.stderr.as_ref().unwrap().len(),
            DEFAULT_EXEC_OUTPUT_TAIL_BYTES
        );
        let unicode = "🦀".repeat(3000);
        let tail = CommandOutput::capture(unicode.as_bytes(), b"\xfffailure", &[])
            .0
            .unwrap();
        assert!(tail.stdout.as_ref().unwrap().len() <= DEFAULT_EXEC_OUTPUT_TAIL_BYTES);
        assert!(tail.stdout_truncated);
        assert_eq!(tail.stderr.as_deref(), Some("�failure"));
        assert!(CommandOutput::capture(b"", b"", &[]).0.is_none());
    }

    #[test]
    fn contextual_errors_preserve_the_command_source_and_output() {
        let error = anyhow::Error::new(CheckpointError::Command {
            action: "push".into(),
            status: "non-zero exit".into(),
            output: CommandOutput::capture(b"", b"remote: rejected", &[]),
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
