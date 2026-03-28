pub mod sandbox;

pub mod read_guard;

pub mod reconnect;

pub mod sandbox_provider;

pub mod sandbox_record;

pub mod worktree;

#[cfg(feature = "ssh")]
pub(crate) mod ssh_common;

#[cfg(feature = "local")]
pub mod local;

#[cfg(feature = "docker")]
pub mod docker;

#[cfg(feature = "sprites")]
pub mod sprites;

#[cfg(feature = "ssh")]
pub mod ssh;

#[cfg(feature = "exe")]
pub mod exe;

#[cfg(feature = "daytona")]
pub mod daytona;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use sandbox::{
    DirEntry, ExecResult, GitRunInfo, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback,
    format_lines_numbered, git_push_via_exec, setup_git_via_exec, shell_quote,
};

pub use read_guard::ReadBeforeWriteSandbox;

pub use sandbox_provider::SandboxProvider;

pub use worktree::{WorktreeConfig, WorktreeEvent, WorktreeEventCallback, WorktreeSandbox};

#[cfg(feature = "local")]
pub use local::LocalSandbox;

#[cfg(feature = "docker")]
pub use docker::{DockerSandbox, DockerSandboxConfig};

pub use sandbox_record::{SandboxRecord, SandboxRecordExt};
