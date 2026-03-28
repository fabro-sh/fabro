// Re-export all sandbox types from fabro-sandbox.
pub use fabro_sandbox::{
    DirEntry, ExecResult, GrepOptions, Sandbox, SandboxEvent, SandboxEventCallback, WorktreeConfig,
    WorktreeEvent, WorktreeEventCallback, WorktreeSandbox, format_lines_numbered, shell_quote,
};

// Re-export the delegate_sandbox! macro at crate root so existing
// `crate::delegate_sandbox!` invocations continue to work.
pub use fabro_sandbox::delegate_sandbox;
