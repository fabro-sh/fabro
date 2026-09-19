//! Fabro's adapters over Petri, the workflow engine Fabro runs its workflows
//! on.
//!
//! This crate is the one place Fabro touches Petri. Every other Fabro crate
//! reaches the engine through the types and functions exported here and never
//! depends on a Petri package itself. That keeps the engine's API surface in
//! one crate, so a Petri pin move is a change to this crate alone.
//!
//! What lives here, as the integration plan lands it:
//!
//! - [`SqliteRunStore`]: Petri's run store over Fabro's SQLite database, so a
//!   run's records are its source of truth in Fabro's tables;
//! - [`runtime`]: the Petri runtime Fabro assembles, at create time and at
//!   execution;
//! - [`check`]: Petri compiles a workflow version's bundle at create time, and
//!   its diagnostics come back in a shape Fabro maps onto its own;
//! - [`admission`]: the admitted graphs in Fabro's blob store, named on the run
//!   spec;
//! - [`engine`]: a run executed by Petri, started or resumed, in the run's
//!   worker process over the HTTP store (or in the server process under its
//!   test override), with the outcome read from its record;
//! - [`interview`]: Petri's interviewer over Fabro's questions API and the
//!   worker's control channel, so a human gate's question reaches the same
//!   places a legacy stage's does and its answer comes back the same way;
//! - [`secrets`]: Petri's secret provider over Fabro's vault, so a `{{
//!   secrets.NAME }}` reference resolves from the vault at spawn and is masked
//!   in every record;
//! - [`blobs`]: Petri's output store over Fabro's blob table, so a large stage
//!   value lives in `blobs` under `blob://sha256/<hex>`;
//! - [`HttpRunStore`]: the same store as a run's worker process reaches it,
//!   over the server's API with the worker's token and its launch id as the
//!   lease owner;
//! - [`projection`] and [`projector`]: the view of a Petri run, folded from its
//!   records and Fabro's platform records, and the pass that writes it after
//!   each committed record;
//! - [`hooks`]: Fabro's `ExecutionHooks`, the checkpoint commit in
//!   `prepare_result` and its platform record in `transition`, around Petri's
//!   own hook service for `[[run.hooks]]`;
//! - [`checkpoint`]: the Git snapshots of a run's workspaces, on the host or
//!   inside a Docker or Daytona sandbox, and the snapshot repository they are
//!   published to;
//! - [`recovery`]: the resume-on-restart protocol, which brings every live
//!   workspace to the snapshot its durable state names: a host workspace before
//!   the run goes back to a worker, a sandbox workspace in the worker when its
//!   scope is acquired;
//! - [`platform_records`]: Fabro's platform records as the adapters reach them,
//!   in the server's database or over its API from a worker;
//! - [`host_tools`]: Fabro's run tools on every native agent session of a run,
//!   through Petri's `HostTools` capability;
//! - [`controls`]: the controls Fabro drives on a live run (pause, unpause,
//!   steer, cancel), over Petri's control service;
//! - [`fork`]: a run seeded from another's records up to a checkpoint's
//!   position, over Petri's `host::fork_from`, with the kept checkpoints, their
//!   snapshots and the run branch carried over: what rewind, fork and retry are
//!   built on.
//!
//! The Petri packages are pinned by revision in the workspace `Cargo.toml`
//! under `petri_*` keys.

pub mod admission;
pub mod blobs;
pub mod check;
pub mod checkpoint;
pub mod controls;
pub mod engine;
pub mod fork;
pub mod hooks;
pub mod host_tools;
pub mod http_store;
pub mod interview;
pub mod petri;
pub mod platform_records;
pub mod projection;
pub mod projector;
pub mod recovery;
pub mod run_store;
pub mod runtime;
pub mod secrets;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod workspace;

pub use http_store::HttpRunStore;
pub use run_store::SqliteRunStore;
