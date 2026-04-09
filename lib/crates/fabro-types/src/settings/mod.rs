//! v2 namespaced config schema plus transitional runtime shapes.
//!
//! The authoritative config schema lives in [`v2`] — it is the namespaced
//! parse tree that `_version = 1` TOML files decode into. Value-language
//! helpers, the merge matrix, and strict unknown-key validation all live
//! there.
//!
//! The submodules `hook`, `mcp`, `project`, `run`, `sandbox`, `server`,
//! and `user` still hold **runtime shapes** that downstream crates
//! (fabro-workflow, fabro-sandbox, fabro-mcp, fabro-hooks) consume at
//! execution time. Stage 6.1 deleted the flat `Settings` parse path;
//! Stage 6.2 deleted the `bridge_to_old` catch-all converter; Stage 6.3b
//! deleted the legacy flat `Settings` struct itself, its inherent
//! helpers, and its `Combine`-driven layering. Narrow v2→runtime helpers
//! live in [`v2::to_runtime`] and build these runtime shapes from
//! specific v2 subtrees on demand.
//!
//! A follow-up pass will either promote these runtime shapes into their
//! owning consumer crates or replace their call sites with v2-native
//! accessors, at which point this module goes away.

pub mod run;
pub mod sandbox;
pub mod server;
pub mod v2;

pub use run::{
    ArtifactsSettings, CheckpointSettings, GitHubSettings, LlmSettings, MergeStrategy,
    PullRequestSettings, SetupSettings,
};
pub use sandbox::{
    DaytonaNetwork, DaytonaSettings, DaytonaSnapshotSettings, DockerfileSource,
    LocalSandboxSettings, SandboxSettings, WorktreeMode,
};
pub use server::{
    ApiAuthStrategy, ApiSettings, ArtifactStorageBackend, ArtifactStorageSettings, AuthProvider,
    AuthSettings, FeaturesSettings, GitAuthorSettings, GitProvider, GitSettings, LogSettings,
    SlackSettings, TlsSettings, WebSettings, WebhookSettings, WebhookStrategy,
};
// v2 top-level re-exports. Stage 6.5 of the settings TOML redesign
// promoted the v2 namespaced parse tree to be the primary API surface;
// consumers can now write `fabro_types::settings::SettingsFile` /
// `fabro_types::settings::InterpString` / `fabro_types::settings::Duration`
// without the `::v2::` prefix. The `v2` module itself stays until the
// remaining legacy files under `settings/{project,run,server,...}.rs`
// are deleted in a follow-up pass, because the v2 submodules and the
// legacy submodules share those file names.
pub use v2::{
    CURRENT_VERSION, CliLayer, Duration, FeaturesLayer, InterpString, ModelRef, ParseDurationError,
    ParseError, ParseModelRefError, ParseSizeError, ProjectLayer, Provenance, ResolveEnvError,
    Resolved, ResolvedModelRef, RunLayer, SchemaVersion, ServerLayer, SettingsFile, Size,
    SpliceArray, SpliceArrayError, VersionError, WorkflowLayer, parse_settings_file,
    validate_version,
};
