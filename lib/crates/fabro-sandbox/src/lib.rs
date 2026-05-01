pub mod config;
pub mod error;
pub mod sandbox;
pub mod sandbox_spec;

#[cfg(any(feature = "docker", feature = "daytona"))]
mod clone_source;

pub mod read_guard;

#[cfg(any(feature = "docker", feature = "daytona", test))]
pub mod redact;

pub mod reconnect;

pub mod sandbox_provider;

pub mod sandbox_record;

pub mod worktree;

pub mod local;

#[cfg(feature = "docker")]
pub mod docker;

#[cfg(feature = "daytona")]
pub mod daytona;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(feature = "docker")]
pub use docker::{DockerSandbox, DockerSandboxOptions};
pub use error::{Error, Result};
pub use local::LocalSandbox;
pub use read_guard::ReadBeforeWriteSandbox;
pub use sandbox::{
    CommandOutputCallback, DEFAULT_EXEC_OUTPUT_TAIL_BYTES, DirEntry, ExecResult,
    ExecStreamingResult, GitRunInfo, GitSetupIntent, GrepOptions, Sandbox, SandboxEvent,
    SandboxEventCallback, format_lines_numbered, git_push_via_exec, redacted_output_tail,
    setup_git_via_exec, shell_quote,
};
pub use sandbox_provider::SandboxProvider;
pub use sandbox_record::SandboxRecord;
pub use sandbox_spec::SandboxSpec;
pub use worktree::{WorktreeEvent, WorktreeEventCallback, WorktreeOptions, WorktreeSandbox};
