/// Convert a Duration's milliseconds to u64, saturating on overflow.
pub(crate) fn millis_u64(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Save a value as pretty-printed JSON to a file.
pub(crate) fn save_json<T: serde::Serialize>(
    value: &T,
    path: &std::path::Path,
    label: &str,
) -> error::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| error::FabroError::Checkpoint(format!("{label} serialize failed: {e}")))?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load a value from a JSON file.
pub(crate) fn load_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    label: &str,
) -> error::Result<T> {
    let data = std::fs::read_to_string(path)?;
    serde_json::from_str(&data)
        .map_err(|e| error::FabroError::Checkpoint(format!("{label} deserialize failed: {e}")))
}

pub mod artifact;
pub mod asset_snapshot;
pub mod checkpoint;
pub mod cli;
pub mod conclusion;
pub mod condition;
pub mod context;
pub mod daytona_sandbox;
pub mod devcontainer_bridge;
pub mod engine;
pub mod error;
pub mod event;
pub mod git;
pub mod graph;
pub mod handler;
pub mod hook;
pub mod interviewer;
pub mod manifest;
pub mod outcome;
pub mod parser;
pub mod preamble;
pub mod pull_request;
pub mod retro;
pub mod retro_agent;
pub mod run_status;
pub mod sandbox_record;
pub mod stylesheet;
pub mod transform;
pub mod validation;
pub mod workflow;
