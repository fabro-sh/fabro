pub mod config;
pub mod error;
#[cfg(any(feature = "docker", feature = "daytona"))]
pub mod from_environment;
pub mod provider;
pub mod sandbox;
pub mod sandbox_spec;

#[cfg(any(feature = "docker", feature = "daytona"))]
mod clone_source;

mod git_retry;

#[cfg(any(feature = "docker", feature = "daytona", test))]
mod managed_labels;

mod push_credentials;

pub mod redact;

pub mod details;

pub mod reconnect;

pub mod terminal;

pub mod local;

#[cfg(feature = "docker")]
pub mod docker;

#[cfg(feature = "daytona")]
pub mod daytona;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use details::sandbox_details;
#[cfg(feature = "docker")]
pub use docker::{DockerSandbox, DockerSandboxOptions};
pub use error::{Error, Result, default_redacted_output_tail, display_for_log};
pub use fabro_github::token_source::{
    InstallationTokenSource, ResolvedToken, TokenProvenance, TokenSnapshot,
};
pub use fabro_types::{RunSandboxInstance, SandboxProviderKind};
pub use git_retry::{
    CredentialContext, GitRetryReason, RetryPlan, classify_failure, retry_git_operation,
};
pub use local::LocalSandbox;
#[cfg(feature = "daytona")]
pub use provider::daytona::DaytonaSandboxProvider;
#[cfg(feature = "docker")]
pub use provider::docker::DockerSandboxProvider;
pub use provider::{
    LocalSandboxProvider, SandboxCreateSpec, SandboxLookupError, SandboxProvider,
    SandboxProviderRegistry,
};
pub use push_credentials::RefreshErrorKind;
pub use reconnect::{reconnect, reconnect_for_run, reconnect_for_run_with_callback};
pub use sandbox::{
    CommandOutputCallback, DEFAULT_EXEC_OUTPUT_TAIL_BYTES, DirEntry, ExecResult,
    ExecStreamingRequest, ExecStreamingResult, GitRunInfo, GitSetupIntent, GrepOptions,
    PushAttempt, PushError, PushReport, RefreshOutcome, RemoteCredentialAction, Sandbox,
    SandboxActivation, SandboxEvent, SandboxEventCallback, SandboxFile, StderrCollector,
    StdioProcess, StdioProcessHandle, StdioProcessTermination, WalkOptions, format_lines_numbered,
    redacted_output_tail, setup_git_via_exec, shell_quote,
};
pub use sandbox_spec::SandboxSpec;
pub use terminal::{TerminalSession, TerminalSize, open_terminal_for_run};
