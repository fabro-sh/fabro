//! Re-export shim for sandbox settings types.
//!
//! Stage 3 removed the parse-time `SandboxConfig`/`DaytonaConfig` types;
//! callers that still import resolved sandbox types via this module use the
//! re-exports below. Stage 6 deletes this file.

pub use fabro_types::settings::sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, WorktreeMode,
};
