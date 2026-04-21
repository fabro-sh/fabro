//! Internal typed key/value helpers for simple SlateDB-backed records.
//!
//! The split of responsibility is:
//! - [`Record`]: declares the key prefix, id type, and codec for one persisted
//!   type.
//! - [`RecordId`]: converts the typed id to and from key segments.
//! - [`Repository`]: performs the generic get/put/delete/scan/gc operations.
//! - [`transaction`]: batches multiple typed writes into one atomic SlateDB
//!   write.
//!
//! Production callers should add a named domain store on top of this layer
//! rather than exposing `Repository<R>` directly. See `slate/auth_codes.rs`,
//! `slate/auth_tokens.rs`, `slate/blob_store.rs`, and
//! `slate/run_catalog_index.rs` for the intended pattern.

mod codec;
mod record_id;
mod repository;
mod transaction;

pub(crate) use codec::{Codec, JsonCodec, MarkerCodec, RawBytesCodec};
pub(crate) use repository::Repository;
pub(crate) use transaction::transaction;

use crate::Result;

pub(crate) trait Record: Sized + Send + Sync + 'static {
    type Id: RecordId;
    type Codec: Codec<Self>;

    const PREFIX: &'static str;

    fn id(&self) -> Self::Id;
}

pub(crate) trait RecordId: Sized {
    fn key_segments(&self) -> Vec<String>;

    fn from_key_segments(segs: &[&str]) -> Result<Self>;
}
