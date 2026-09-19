//! Petri's store vocabulary, re-exported for the Fabro crates that hold a
//! Petri run handle or answer for one (the server's worker endpoints) without
//! depending on the Petri packages themselves. Only this crate names them in
//! its `Cargo.toml`.

use petri_execution::events;
pub use petri_store::{
    Access, Digest, ExecutionId, LogId, OwnerId, Record, RunKey, RunLogs, RunStore, StoreError,
};

/// The version of Petri's public event contract this build serves on the
/// run stream: every `petri` item of `GET /runs/{id}/events` follows it.
pub const EVENT_CONTRACT_VERSION: u32 = events::EVENT_CONTRACT_VERSION;
