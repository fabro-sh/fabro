#[cfg(feature = "docker")]
use bollard::errors::Error as BollardError;
use fabro_util::error::{collect_causes, render_with_causes};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),

    #[error("{message}")]
    Context {
        message: String,
        #[source]
        source:  Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[cfg(feature = "docker")]
    #[error("Failed to connect to Docker daemon")]
    DockerConnect {
        #[source]
        source: BollardError,
    },

    #[cfg(feature = "docker")]
    #[error("Failed to inspect Docker image {image}")]
    DockerImageInspect {
        image:  String,
        #[source]
        source: BollardError,
    },

    #[cfg(feature = "docker")]
    #[error("Failed to pull Docker image {image}")]
    DockerImagePull {
        image:  String,
        #[source]
        source: BollardError,
    },

    #[error(
        "{label} failed (exit {exit_code}, timed_out={timed_out}, duration_ms={duration_ms}) - hint: {hint}",
        hint = classify_exec_failure(stderr)
            .or_else(|| classify_exec_failure(stdout))
            .unwrap_or("unclassified")
    )]
    Exec {
        label:       String,
        exit_code:   i32,
        timed_out:   bool,
        duration_ms: u64,
        stderr:      String,
        stdout:      String,
    },
}

impl Error {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub fn context(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Context {
            message: message.into(),
            source:  Box::new(source),
        }
    }

    pub fn exec(
        label: impl Into<String>,
        exit_code: i32,
        timed_out: bool,
        duration_ms: u64,
        stderr: impl Into<String>,
        stdout: impl Into<String>,
    ) -> Self {
        Self::Exec {
            label: label.into(),
            exit_code,
            timed_out,
            duration_ms,
            stderr: stderr.into(),
            stdout: stdout.into(),
        }
    }

    pub fn exec_stderr(&self) -> Option<&str> {
        match self {
            Self::Exec { stderr, .. } => Some(stderr),
            _ => None,
        }
    }

    pub fn exec_stdout(&self) -> Option<&str> {
        match self {
            Self::Exec { stdout, .. } => Some(stdout),
            _ => None,
        }
    }

    pub fn exec_label(&self) -> Option<&str> {
        match self {
            Self::Exec { label, .. } => Some(label),
            _ => None,
        }
    }

    pub fn exec_exit_code(&self) -> Option<i32> {
        match self {
            Self::Exec { exit_code, .. } => Some(*exit_code),
            _ => None,
        }
    }

    pub fn exec_timed_out(&self) -> Option<bool> {
        match self {
            Self::Exec { timed_out, .. } => Some(*timed_out),
            _ => None,
        }
    }

    pub fn exec_duration_ms(&self) -> Option<u64> {
        match self {
            Self::Exec { duration_ms, .. } => Some(*duration_ms),
            _ => None,
        }
    }

    #[cfg(feature = "docker")]
    pub fn docker_connect(source: BollardError) -> Self {
        Self::DockerConnect { source }
    }

    #[cfg(feature = "docker")]
    pub fn docker_image_inspect(image: impl Into<String>, source: BollardError) -> Self {
        Self::DockerImageInspect {
            image: image.into(),
            source,
        }
    }

    #[cfg(feature = "docker")]
    pub fn docker_image_pull(image: impl Into<String>, source: BollardError) -> Self {
        Self::DockerImagePull {
            image: image.into(),
            source,
        }
    }

    pub fn causes(&self) -> Vec<String> {
        collect_causes(self)
    }

    pub fn display_with_causes(&self) -> String {
        render_with_causes(&self.to_string(), &self.causes())
    }
}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

pub(crate) fn classify_exec_failure(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("could not read username") || lower.contains("terminal prompts disabled") {
        Some(
            "no credentials in origin URL - check that the sandbox forwarded \
             GITHUB_APP_PRIVATE_KEY (or GITHUB_TOKEN) and that refresh_push_credentials succeeded",
        )
    } else if lower.contains("permission to") && lower.contains("denied") {
        Some(
            "github denied the push - installation token lacks contents:write \
             on this repo, or a branch protection / push ruleset is rejecting the ref",
        )
    } else if lower.contains("protected branch")
        || lower.contains("ruleset")
        || lower.contains("rejected")
    {
        Some("github rejected the ref - likely a branch protection rule or push ruleset")
    } else if lower.contains("authentication failed") || lower.contains("invalid username") {
        Some("github authentication failed - installation token may be expired or wrong scope")
    } else if lower.contains("could not resolve host") || lower.contains("network is unreachable") {
        Some("network failure inside sandbox - check DNS / egress from the run container")
    } else if lower.contains("repository not found") {
        Some("github 404 - the App installation may not include this repo")
    } else if lower.contains("no such remote") && lower.contains("origin") {
        Some("origin remote missing - push credentials could not be installed")
    } else if lower.contains("not a git repository")
        || lower.contains("does not appear to be a git repository")
    {
        Some("git repository unavailable in sandbox working directory")
    } else {
        None
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_display_is_log_safe() {
        let stderr = "fatal: unable to access \
                      'https://x-access-token:ghs_xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA@github.com/owner/repo/':\n\
                      remote: Permission to owner/repo.git denied\n\
                      identity ~/.ssh/id_rsa_work";
        let error = Error::exec(
            "git push origin refs/heads/run",
            128,
            false,
            210,
            stderr,
            "",
        );
        let rendered = error.to_string();

        for forbidden in [
            "fatal:",
            "remote:",
            "x-access-token",
            "ghs_xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA",
            "~/.ssh",
            "id_rsa_work",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "Display leaked {forbidden:?}: {rendered}"
            );
        }
        assert!(rendered.contains("git push origin refs/heads/run"));
        assert!(rendered.contains("exit 128"));
        assert!(rendered.contains("timed_out=false"));
        assert!(rendered.contains("duration_ms=210"));
        assert!(rendered.contains("hint:"));
    }

    #[test]
    fn exec_accessors_return_stored_values() {
        let error = Error::exec("git push", 1, true, 5000, "stored stderr", "stored stdout");

        assert_eq!(error.exec_label(), Some("git push"));
        assert_eq!(error.exec_exit_code(), Some(1));
        assert_eq!(error.exec_timed_out(), Some(true));
        assert_eq!(error.exec_duration_ms(), Some(5000));
        assert_eq!(error.exec_stderr(), Some("stored stderr"));
        assert_eq!(error.exec_stdout(), Some("stored stdout"));
    }

    #[test]
    fn non_exec_accessors_return_none() {
        let message = Error::message("plain");
        assert_eq!(message.exec_label(), None);
        assert_eq!(message.exec_exit_code(), None);
        assert_eq!(message.exec_timed_out(), None);
        assert_eq!(message.exec_duration_ms(), None);
        assert_eq!(message.exec_stderr(), None);
        assert_eq!(message.exec_stdout(), None);

        let source = std::io::Error::other("source");
        let context = Error::context("context", source);
        assert_eq!(context.exec_label(), None);
        assert_eq!(context.exec_stderr(), None);
    }

    #[test]
    fn classify_exec_failure_documents_known_branches() {
        let cases = [
            (
                "fatal: could not read Username for 'https://github.com'",
                "no credentials in origin URL",
            ),
            (
                "remote: Permission to owner/repo.git denied to fabro-app[bot].",
                "github denied the push",
            ),
            (
                "remote: error: GH013: Repository rule violations found due to ruleset",
                "github rejected the ref",
            ),
            (
                "fatal: Authentication failed for 'https://github.com/owner/repo'",
                "github authentication failed",
            ),
            (
                "fatal: could not resolve host: github.com",
                "network failure",
            ),
            ("remote: Repository not found.", "github 404"),
            ("error: No such remote 'origin'", "origin remote missing"),
            ("fatal: not a git repository", "git repository unavailable"),
        ];

        for (stderr, expected) in cases {
            let hint = classify_exec_failure(stderr).expect(stderr);
            assert!(
                hint.contains(expected),
                "expected {hint:?} to contain {expected:?}"
            );
        }
        assert_eq!(classify_exec_failure("weird new git error"), None);
    }
}
