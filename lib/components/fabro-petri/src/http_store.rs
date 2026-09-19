//! Petri's run store as a run's worker process reaches it: the Petri store
//! crate's `RunStore` and `RunLogs` over the Fabro server's API, with the
//! worker's token. The server side is [`SqliteRunStore`](crate::SqliteRunStore)
//! behind the `/api/v1/runs/{id}/petri/*` endpoints, so the lease and the
//! `(log, seq)` rule are the store's: this layer carries requests and maps
//! replies.
//!
//! # Keys
//!
//! A run key over the API is a Fabro run id, the `{id}` of every endpoint.
//! The worker's token names the one run it may reach; any other key is
//! refused by the server. That is the integration plan's rule that Petri's
//! `run_key` is Fabro's run id.
//!
//! # The lease
//!
//! `Create` and `Write` take the run's writer lease for the handle's
//! `OwnerId` on the server, idempotently: a same-owner reopen in this
//! process shares the live handle, and a same-owner reopen after a lost
//! reply gets the same lease from the server. Another live owner is refused
//! with `Leased`. The lease ends when the last handle of the owner drops
//! (the drop sends `release`, best effort, on the current Tokio runtime),
//! when the server observes the worker exit, or by operator release. Never
//! by timeout. The store awaits every spawned release before its next
//! `open`, so a drop followed by an open observes the release; with no
//! runtime at drop, the server's worker-exit release is the backstop, and
//! the drop says so in the log.
//!
//! # The owner
//!
//! A store built with [`HttpRunStore::for_worker`] names one owner for the
//! whole process: every `Create` and `Write` takes the lease for the
//! worker's launch id, whatever owner Petri minted for the run runtime that
//! asked. One worker process executes one run, so the lease is the
//! launch's, the worker logs it once at start, and the server's lease row
//! names the launch that holds it. A store built with [`HttpRunStore::new`]
//! passes Petri's owner through unchanged.
//!
//! # Lost replies
//!
//! Every call is one request. A reply that never arrives (a transport error,
//! or the client's request timeout) is retried by resending the same
//! request a bounded number of times, with [`LOST_REPLY_RETRY_DELAYS`]
//! between attempts. That is safe because every request is idempotent on the
//! server: a repeated record at a taken seq is accepted, a blob write is
//! content-addressed, an open by the owner that holds the lease shares it,
//! and a release by an owner that no longer holds the lease is a no-op. A
//! `Create` whose reply was lost may find the run exists on the resend; the
//! store then takes the run with `Write` for the same owner, which the lease
//! rule makes the same lease. A reply that did arrive is never retried: the
//! server's answer, error or not, is the store's answer.
//!
//! # Errors
//!
//! The server names each store error with a machine-readable `code`
//! (`petri_run_exists`, `petri_run_not_found`, `petri_run_leased` with the
//! holder under `meta.owner`, `petri_stale_owner`, `petri_read_only`,
//! `petri_record_conflict` with the position under `meta.log` and
//! `meta.seq`), and the store maps each back to its `StoreError` variant.
//! Anything else, including a lost reply after the last retry, is
//! `StoreError::Backend`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;
use std::{fmt, mem, ptr};

use fabro_api::types::{PetriAccess, PetriAppendRequest, PetriOpenRequest, PetriRecord};
use fabro_client::{Client, api_failure_for};
use fabro_types::{BlobHash, RunId};
use petri_store::{Access, Digest, LogId, OwnerId, Record, RunKey, RunLogs, RunStore, StoreError};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, warn};

use crate::run_store::{log_id_text, parse_log_id};

/// The waits between attempts when a reply is lost: one request, then up
/// to three resends.
pub const LOST_REPLY_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_secs(2),
];

/// Petri's run store over the Fabro server's API.
pub struct HttpRunStore {
    shared: Arc<Shared>,
}

impl fmt::Debug for HttpRunStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRunStore")
            .field("server", &self.shared.client.base_url())
            .finish_non_exhaustive()
    }
}

/// What the store and every handle it opens share.
struct Shared {
    client:   Client,
    /// The owner every writer open takes the lease for, when the store is
    /// a worker's; `None` passes Petri's owner through.
    owner:    Option<OwnerId>,
    /// The writer handle alive in this process per run and owner, so a
    /// same-owner reopen shares it and the lease lasts while any handle
    /// does.
    live:     Mutex<HashMap<(RunKey, OwnerId), Weak<HttpRunLogs>>>,
    /// The releases dropped handles spawned, awaited before the next open.
    releases: Mutex<Vec<JoinHandle<()>>>,
}

impl HttpRunStore {
    /// A store over a client that carries the worker's token, taking each
    /// lease for the owner Petri names.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self::build(client, None)
    }

    /// A worker's store: every lease is taken for `owner`, the worker's
    /// launch id, whatever owner Petri names.
    #[must_use]
    pub fn for_worker(client: Client, owner: OwnerId) -> Self {
        Self::build(client, Some(owner))
    }

    fn build(client: Client, owner: Option<OwnerId>) -> Self {
        Self {
            shared: Arc::new(Shared {
                client,
                owner,
                live: Mutex::default(),
                releases: Mutex::default(),
            }),
        }
    }

    /// The writer handle for `owner`, once the lease is taken: the live one
    /// when this owner already holds a handle here, else a new one.
    fn writer(
        &self,
        key: &RunKey,
        run_id: RunId,
        owner: OwnerId,
        locator: String,
    ) -> Arc<HttpRunLogs> {
        let mut live = lock(&self.shared.live);
        let slot = (key.clone(), owner.clone());
        if let Some(handle) = live.get(&slot).and_then(Weak::upgrade) {
            return handle;
        }
        let handle = Arc::new(HttpRunLogs {
            shared: self.shared.clone(),
            run_id,
            key: key.clone(),
            owner: Some(owner),
            locator,
        });
        live.insert(slot, Arc::downgrade(&handle));
        handle
    }
}

impl Shared {
    /// Where a run lives, for messages before the server has said.
    fn locator(&self, key: &RunKey) -> String {
        format!("Fabro server {}, run `{key}`", self.client.base_url())
    }

    /// The Fabro run id a key names, which is the `{id}` of every request.
    fn run_id(&self, key: &RunKey) -> Result<RunId, StoreError> {
        key.as_str().parse::<RunId>().map_err(|cause| {
            StoreError::backend(
                self.locator(key),
                "name the run",
                format!("a Petri run key over Fabro's API is a Fabro run id: {cause}"),
            )
        })
    }

    /// Await every release a dropped handle spawned, so what follows sees
    /// the lease as the drops left it.
    async fn drain_releases(&self) {
        let pending = mem::take(&mut *lock(&self.releases));
        for release in pending {
            // A release task never panics: it reports its own failure.
            let _ = release.await;
        }
    }

    /// Run `request` until a reply arrives: a lost reply is resent after
    /// each of [`LOST_REPLY_RETRY_DELAYS`], and the last loss is the error.
    async fn until_replied<T, F, Fut>(
        &self,
        key: &RunKey,
        action: &'static str,
        mut request: F,
    ) -> Result<T, anyhow::Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, anyhow::Error>>,
    {
        let mut attempt = 0;
        loop {
            match request().await {
                Ok(value) => return Ok(value),
                Err(error) if reply_lost(&error) && attempt < LOST_REPLY_RETRY_DELAYS.len() => {
                    let delay = LOST_REPLY_RETRY_DELAYS[attempt];
                    attempt += 1;
                    warn!(
                        run_id = %key,
                        action,
                        attempt,
                        delay_ms = delay.as_millis(),
                        error = %error,
                        "Petri store reply lost; resending the request"
                    );
                    time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Map a request's failure to the store error the server named, or a
    /// backend error for anything else. `log` is the request's log, the
    /// fallback for a conflict whose `meta` does not name one.
    fn store_error(
        &self,
        key: &RunKey,
        action: &'static str,
        log: Option<&LogId>,
        error: anyhow::Error,
    ) -> StoreError {
        let Some(failure) = api_failure_for(&error) else {
            return StoreError::backend(self.locator(key), action, error);
        };
        let meta = |member: &str| {
            failure
                .meta
                .as_ref()
                .and_then(|meta| meta.get(member).cloned())
        };
        match failure.code.as_deref() {
            Some("petri_run_exists") => StoreError::Exists {
                key:     key.clone(),
                locator: self.locator(key),
            },
            Some("petri_run_not_found") => StoreError::NotFound {
                key:     key.clone(),
                locator: self.locator(key),
            },
            Some("petri_run_leased") => StoreError::Leased {
                locator: self.locator(key),
                owner:   OwnerId::new(
                    meta("owner")
                        .and_then(|owner| owner.as_str().map(ToOwned::to_owned))
                        .unwrap_or_else(|| "<unnamed>".to_string()),
                ),
            },
            Some("petri_stale_owner") => StoreError::StaleOwner,
            Some("petri_read_only") => StoreError::ReadOnly,
            Some("petri_record_conflict") => {
                let named = meta("log")
                    .and_then(|log| log.as_str().and_then(parse_log_id))
                    .or_else(|| log.copied());
                let seq = meta("seq").and_then(|seq| seq.as_u64());
                match (named, seq) {
                    (Some(log), Some(seq)) => StoreError::Conflict { log, seq },
                    _ => StoreError::backend(self.locator(key), action, error),
                }
            }
            _ => StoreError::backend(self.locator(key), action, error),
        }
    }

    /// End the lease of `key` for `owner` on the server: what a dropped
    /// handle does. A lease that already moved is left alone by the server.
    async fn release(&self, key: &RunKey, run_id: RunId, owner: &OwnerId) {
        let released = self
            .until_replied(key, "release the run's lease", || {
                self.client.release_petri_run(&run_id, owner.as_str())
            })
            .await;
        match released {
            Ok(()) => debug!(run_id = %key, owner = %owner, "Petri run lease released at drop"),
            Err(error) => warn!(
                run_id = %key,
                owner = %owner,
                error = %error,
                "Petri run lease not released at drop; the server releases it when the worker exits"
            ),
        }
    }
}

/// Whether a request failed without a reply from the server: a transport
/// error or the client's request timeout. A reply, whatever its status,
/// carries an `ApiFailure`.
fn reply_lost(error: &anyhow::Error) -> bool {
    api_failure_for(error).is_none()
}

#[async_trait::async_trait]
impl RunStore for HttpRunStore {
    async fn open(&self, key: &RunKey, access: Access) -> Result<Arc<dyn RunLogs>, StoreError> {
        let shared = &self.shared;
        shared.drain_releases().await;
        let run_id = shared.run_id(key)?;
        let (api_access, owner) = match &access {
            Access::Create { owner } => (PetriAccess::Create, Some(owner)),
            Access::Write { owner } => (PetriAccess::Write, Some(owner)),
            Access::Read => (PetriAccess::Read, None),
        };
        // A worker's store leases for its launch, not for the owner Petri
        // minted for this run runtime.
        let owner = owner.map(|named| shared.owner.as_ref().unwrap_or(named));
        let request = PetriOpenRequest {
            access: api_access,
            owner:  owner.map(|owner| owner.as_str().to_string()),
        };
        let mut sent = 0_usize;
        let opened = shared
            .until_replied(key, "open the run", || {
                sent += 1;
                shared.client.open_petri_run(&run_id, request.clone())
            })
            .await;
        let opened = match (opened, owner) {
            // A `Create` whose first reply was lost may have created the
            // run: the resend finds it and takes it as its owner.
            (Err(error), Some(owner))
                if sent > 1
                    && matches!(request.access, PetriAccess::Create)
                    && api_failure_for(&error).is_some_and(|failure| {
                        failure.code.as_deref() == Some("petri_run_exists")
                    }) =>
            {
                debug!(run_id = %key, owner = %owner, "Petri run created by a resend; taking it");
                let retake = PetriOpenRequest {
                    access: PetriAccess::Write,
                    owner:  Some(owner.as_str().to_string()),
                };
                shared
                    .until_replied(key, "open the run", || {
                        shared.client.open_petri_run(&run_id, retake.clone())
                    })
                    .await
            }
            (opened, _) => opened,
        };
        let opened =
            opened.map_err(|error| shared.store_error(key, "open the run", None, error))?;
        debug!(
            run_id = %key,
            access = ?request.access,
            owner = owner.map(OwnerId::as_str),
            "Petri run opened over the API"
        );
        match owner {
            Some(owner) => Ok(self.writer(key, run_id, owner.clone(), opened.locator)),
            None => Ok(Arc::new(HttpRunLogs {
                shared: shared.clone(),
                run_id,
                key: key.clone(),
                owner: None,
                locator: opened.locator,
            })),
        }
    }
}

/// One run on the server, opened. A writer handle carries the owner it was
/// opened with; a reader handle refuses every mutation.
struct HttpRunLogs {
    shared:  Arc<Shared>,
    run_id:  RunId,
    key:     RunKey,
    owner:   Option<OwnerId>,
    locator: String,
}

impl HttpRunLogs {
    fn owner(&self) -> Result<&OwnerId, StoreError> {
        self.owner.as_ref().ok_or(StoreError::ReadOnly)
    }

    fn backend(
        &self,
        action: &'static str,
        cause: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> StoreError {
        StoreError::backend(self.locator.clone(), action, cause)
    }

    /// A record as the wire carries it: its JSON must be an object.
    fn wire(&self, record: &Record) -> Result<PetriRecord, StoreError> {
        match &record.record {
            Value::Object(map) => Ok(PetriRecord {
                seq:         record.seq,
                recorded_at: record.recorded_at,
                record:      map.clone(),
            }),
            _ => Err(self.backend(
                "encode a record",
                format!("record at seq {} is not a JSON object", record.seq),
            )),
        }
    }

    /// A wire record back into the record it was, checked against the seq
    /// and recorded_at the server lifted beside it.
    fn decode(&self, wire: PetriRecord) -> Result<Record, StoreError> {
        let record = Record::from_value(Value::Object(wire.record))
            .map_err(|cause| self.backend("decode a stored record", cause))?;
        if record.seq != wire.seq || record.recorded_at != wire.recorded_at {
            return Err(self.backend(
                "decode a stored record",
                format!(
                    "the server lifted seq {} and recorded_at {} beside a record carrying seq {} and recorded_at {}",
                    wire.seq, wire.recorded_at, record.seq, record.recorded_at
                ),
            ));
        }
        Ok(record)
    }
}

impl Drop for HttpRunLogs {
    fn drop(&mut self) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        {
            let mut live = lock(&self.shared.live);
            let slot = (self.key.clone(), owner.clone());
            let this: *const Self = self;
            if live
                .get(&slot)
                .is_some_and(|weak| ptr::eq(weak.as_ptr(), this))
            {
                live.remove(&slot);
            }
        }
        match Handle::try_current() {
            Ok(runtime) => {
                let shared = self.shared.clone();
                let key = self.key.clone();
                let run_id = self.run_id;
                let release = runtime.spawn(async move {
                    shared.release(&key, run_id, &owner).await;
                });
                lock(&self.shared.releases).push(release);
            }
            Err(_) => {
                warn!(
                    run_id = %self.key,
                    owner = %owner,
                    "Petri run lease not released at drop: no async runtime; the server releases it when the worker exits"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl RunLogs for HttpRunLogs {
    fn locator(&self) -> String {
        self.locator.clone()
    }

    async fn append(&self, log: &LogId, records: &[Record]) -> Result<(), StoreError> {
        let owner = self.owner()?;
        let body = PetriAppendRequest {
            owner:   owner.as_str().to_string(),
            records: records
                .iter()
                .map(|record| self.wire(record))
                .collect::<Result<Vec<_>, _>>()?,
        };
        let log_text = log_id_text(log);
        self.shared
            .until_replied(&self.key, "append records", || {
                self.shared
                    .client
                    .append_petri_records(&self.run_id, &log_text, body.clone())
            })
            .await
            .map_err(|error| {
                self.shared
                    .store_error(&self.key, "append records", Some(log), error)
            })
    }

    async fn read(&self, log: &LogId) -> Result<Vec<Record>, StoreError> {
        let log_text = log_id_text(log);
        let records = self
            .shared
            .until_replied(&self.key, "read a log", || {
                self.shared
                    .client
                    .list_petri_records(&self.run_id, &log_text)
            })
            .await
            .map_err(|error| {
                self.shared
                    .store_error(&self.key, "read a log", Some(log), error)
            })?;
        records.into_iter().map(|wire| self.decode(wire)).collect()
    }

    async fn put_blob(&self, bytes: &[u8]) -> Result<Digest, StoreError> {
        let owner = self.owner()?;
        let hash = self
            .shared
            .until_replied(&self.key, "store a blob", || {
                self.shared
                    .client
                    .write_petri_blob(&self.run_id, owner.as_str(), bytes)
            })
            .await
            .map_err(|error| {
                self.shared
                    .store_error(&self.key, "store a blob", None, error)
            })?;
        hash.to_string()
            .parse()
            .map_err(|cause| self.backend("store a blob", cause))
    }

    async fn get_blob(&self, digest: Digest) -> Result<Option<Vec<u8>>, StoreError> {
        let hash: BlobHash = digest
            .to_hex()
            .parse()
            .map_err(|cause| self.backend("read a blob", cause))?;
        let bytes = self
            .shared
            .until_replied(&self.key, "read a blob", || {
                self.shared.client.read_petri_blob(&self.run_id, &hash)
            })
            .await
            .map_err(|error| {
                self.shared
                    .store_error(&self.key, "read a blob", None, error)
            })?;
        Ok(bytes.map(|bytes| bytes.to_vec()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use fabro_client::ApiFailure;
    use fabro_client::error::tag_with_failure;
    use petri_store::ExecutionId;
    use serde_json::json;

    use super::*;

    fn store() -> HttpRunStore {
        HttpRunStore::new(Client::new_no_proxy("http://127.0.0.1:1").expect("a client builds"))
    }

    fn failure(status: u16, code: &str, meta: Option<Value>) -> anyhow::Error {
        tag_with_failure(anyhow::anyhow!("the server said no"), ApiFailure {
            status: fabro_http::StatusCode::from_u16(status).expect("a status"),
            code: Some(code.to_string()),
            meta,
        })
    }

    #[test]
    fn the_server_codes_map_back_to_the_store_errors() {
        let store = store();
        let key = RunKey::new("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let log = LogId::Execution(ExecutionId::new(2));
        let map = |error| store.shared.store_error(&key, "act", Some(&log), error);

        assert!(matches!(
            map(failure(409, "petri_run_exists", None)),
            StoreError::Exists { .. }
        ));
        assert!(matches!(
            map(failure(404, "petri_run_not_found", None)),
            StoreError::NotFound { .. }
        ));
        assert!(matches!(
            map(failure(409, "petri_run_leased", Some(json!({"owner": "first"})))),
            StoreError::Leased { owner, .. } if owner.as_str() == "first"
        ));
        assert!(matches!(
            map(failure(409, "petri_stale_owner", None)),
            StoreError::StaleOwner
        ));
        assert!(matches!(
            map(failure(409, "petri_read_only", None)),
            StoreError::ReadOnly
        ));
        assert!(matches!(
            map(failure(
                409,
                "petri_record_conflict",
                Some(json!({"log": "resources", "seq": 4}))
            )),
            StoreError::Conflict {
                log: LogId::Resources,
                seq: 4,
            }
        ));
        assert!(
            matches!(
                map(failure(409, "petri_record_conflict", Some(json!({"seq": 1})))),
                StoreError::Conflict { log: named, seq: 1 } if named == log
            ),
            "a conflict that names no log is on the request's log"
        );
        assert!(matches!(
            map(failure(500, "petri_store_failed", None)),
            StoreError::Backend { .. }
        ));
        assert!(matches!(
            map(anyhow::anyhow!("connection reset")),
            StoreError::Backend { .. }
        ));
    }

    #[test]
    fn a_reply_is_lost_only_without_an_api_failure() {
        assert!(reply_lost(&anyhow::anyhow!("server request timed out")));
        assert!(!reply_lost(&failure(409, "petri_stale_owner", None)));
    }

    #[test]
    fn a_key_over_the_api_is_a_fabro_run_id() {
        let store = store();
        let error = store
            .shared
            .run_id(&RunKey::new("lifecycle"))
            .expect_err("a plain word is not a run id");
        assert!(matches!(error, StoreError::Backend { .. }), "{error}");
        assert!(
            store
                .shared
                .run_id(&RunKey::new("01ARZ3NDEKTSV4RRFFQ69G5FAV"))
                .is_ok()
        );
    }
}
