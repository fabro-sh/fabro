//! The projector: the view pass that folds a Petri run's committed records
//! into its stored projection, and the wake-up that drives it.
//!
//! # Commit rule
//!
//! Records first. Petri's append (the worker's append endpoint, then
//! `SqliteRunStore::append`) and a platform record's insert are the
//! durability boundaries, and both return before any view work. A view pass
//! then reads what is committed, folds the items past the positions the
//! view last committed, and writes the derived rows in one later
//! transaction together with the new positions: the last event consumed per
//! Petri log, the last platform record consumed, and the delivery sequence
//! (`stream_seq`) it assigned to each item. The view therefore trails a
//! committed record and never leads one. No projection state of Petri's is
//! checkpointed: a pass derives the events past the held positions from
//! the records alone, and every stored view equals a full replay
//! (`replay_run`) of the records it holds.
//!
//! A pass that finds new platform records committed between its read and
//! its write leaves the view alone and runs again, so the `runs` row never
//! moves backwards behind a concurrent lifecycle write.
//!
//! # The live run's cache
//!
//! A pass keeps in memory, per live run, Petri's replay of the run (a
//! `RunReplay`: the coordinator state, each execution's engine state, the
//! projection) and the view as the pass last committed it, so the next
//! pass reads and folds only the records past the ones the view holds and
//! costs the new records, not the run's length. The cache is never a
//! source of facts and never checkpointed: it is dropped when the run
//! records its finish, after ten idle minutes, when the stored view moves
//! under it, when the run is deleted, and with the process, and the first
//! pass after that rebuilds it by a full replay. A pass that commits
//! nothing (a platform record landed under it, or it failed before its
//! view transaction) keeps the events it derived for the next pass, so
//! nothing is derived twice or lost.
//!
//! # Where it runs
//!
//! In the server. [`Projector::signal`] schedules a pass for a run: the
//! server calls it after each committed worker append and, through the run
//! summary store's hook, after each committed platform record; signals
//! that arrive while a pass runs coalesce into one more pass. A signal is a
//! wake-up only, never a source of facts: a signal that is lost costs
//! nothing but latency, because the next signal or the startup pass
//! ([`Projector::startup_pass`]) folds everything the view still trails.
//!
//! # A torn tail
//!
//! A record the store holds that Petri cannot read (a gap in a log, a line
//! that does not decode) fails the replay. The pass then advances no Petri
//! position, folds only the platform records, and reports the run's record
//! as incomplete with the replay's error; `inspect_run` decides
//! completeness once the run has recorded its finish.

mod cache;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fabro_db::DbPool;
use fabro_store::platform_records::{PlatformRecordStore, StoredPlatformRecord, now_ms};
use fabro_store::{RunProjection, RunSummaryStore};
use fabro_types::{RunId, RunStreamItem, RunStreamItemKind};
use fabro_util::error::collect_chain;
use petri_execution::events::{self, EventId, EventSource, RunEvent};
use petri_execution::{Access, CoordinatorEvent, RunKey, RunStore as _, inspect};
use petri_runtime::engine::Event;
use petri_store::StoreError;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time;
use tracing::{debug, info, warn};

use self::cache::{Caches, IDLE, RunCache};
use crate::SqliteRunStore;
use crate::projection::{self, FoldState, Item, RecordHealth, RunView};

/// The positions a view committed: the last event consumed per Petri log,
/// and the last platform record consumed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Positions {
    #[serde(default)]
    pub petri:        Vec<EventId>,
    #[serde(default)]
    pub platform_seq: u64,
}

impl Positions {
    fn held(&self) -> BTreeMap<EventSource, EventId> {
        self.petri.iter().map(|id| (id.source, *id)).collect()
    }

    fn advance(&mut self, id: EventId) {
        match self.petri.iter_mut().find(|held| held.source == id.source) {
            Some(held) => {
                if id > *held {
                    *held = id;
                }
            }
            None => self.petri.push(id),
        }
    }
}

/// What one pass did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassReport {
    pub run_id:           RunId,
    /// The pass found nothing past the committed positions and wrote nothing.
    pub skipped:          bool,
    /// The view was left alone because a platform record landed during the
    /// pass; the projector runs the pass again.
    pub contended:        bool,
    pub petri_events:     usize,
    pub platform_records: usize,
    /// How many of the run's records the pass fed through Petri's
    /// derivation, before the held positions trimmed their events: the
    /// pass's cost. The records past the cache for a live run, the whole
    /// run for a pass that rebuilt it.
    pub replayed_records: usize,
    /// The last delivery sequence the view holds.
    pub stream_seq:       u64,
    pub positions:        Positions,
    pub health:           RecordHealth,
}

/// What the startup pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupReport {
    pub runs:      usize,
    pub projected: usize,
    /// Runs whose pass failed and was left for the next signal.
    pub failed:    usize,
}

/// Why a pass could not run or commit.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("the run's Petri record could not be opened")]
    Open(#[source] StoreError),
    #[error("the projection tables could not be read or written")]
    Database(#[source] sqlx::Error),
    #[error("the platform records could not be read or written")]
    Store(#[source] fabro_store::Error),
    #[error("the view could not be encoded")]
    Encode(#[source] serde_json::Error),
    #[error("the pass was stopped before its view transaction (injected)")]
    Injected,
}

/// The stored view of a run, as the projection tables hold it.
#[derive(Clone)]
struct StoredView {
    view:       RunView,
    positions:  Positions,
    stream_seq: u64,
}

/// A run's pass state under the projector's lock.
#[derive(Default)]
struct Slot {
    running: bool,
    pending: bool,
}

/// The projector over one database: the pool Petri's records are read
/// from, and the pool the view tables (`platform_records`,
/// `petri_projection`, `petri_stream`, `runs`) are read and written on. In
/// the server both are the one database; a test may hand it the run
/// summary store's own pool for the views.
pub struct Projector {
    records:           DbPool,
    pool:              DbPool,
    store:             SqliteRunStore,
    platform:          PlatformRecordStore,
    slots:             Mutex<HashMap<RunId, Slot>>,
    /// One pass at a time per run (a signalled pass and the startup pass
    /// over the same run never interleave their reads and writes), and the
    /// cache each live run's passes continue from.
    pub(crate) caches: Caches,
    /// Test-only: stop the next pass after its reads, before its view
    /// transaction, as a crash there would.
    fault:             AtomicBool,
    /// Sent after each committed pass that wrote stream rows: the run whose
    /// stream grew. A wake-up for the stream's readers, never a source of
    /// facts; a reader that lags re-reads from its cursor.
    committed:         broadcast::Sender<RunId>,
}

impl std::fmt::Debug for Projector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Projector").finish_non_exhaustive()
    }
}

impl Projector {
    /// A projector over `records`, the pool Petri's records live in, and
    /// `views`, the pool the view tables live in; both migrated. The server
    /// passes its one pool twice.
    #[must_use]
    pub fn new(records: DbPool, views: DbPool) -> Arc<Self> {
        Arc::new(Self {
            store: SqliteRunStore::new(records.clone()),
            platform: PlatformRecordStore::new(views.clone()),
            records,
            pool: views,
            slots: Mutex::default(),
            caches: Caches::default(),
            fault: AtomicBool::new(false),
            committed: broadcast::channel(COMMIT_SIGNAL_CAPACITY).0,
        })
    }

    /// A receiver that learns which run's stream grew after each committed
    /// pass. A receiver that falls behind gets `Lagged` and treats it as a
    /// wake-up for every run it follows.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<RunId> {
        self.committed.subscribe()
    }

    /// The run's stream past the cursor: up to `limit` items with
    /// `stream_seq > after`, in `stream_seq` order, each in Fabro's
    /// envelope. `after = 0` reads from the first item.
    pub async fn stream_after(
        &self,
        run_id: RunId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<RunStreamItem>, ProjectError> {
        stream_after(&self.pool, run_id, after, limit).await
    }

    /// Delete everything the store and the view tables hold for the run:
    /// its Petri records and lease, its platform records, its projection
    /// and its stream. The caller has ended the run's worker, so no writer
    /// holds the lease.
    pub async fn delete_run(&self, run_id: RunId) -> Result<(), ProjectError> {
        // Under the run's pass lock: no pass reads the rows being deleted,
        // and no cache outlives them.
        let pass = self.caches.pass_of(run_id);
        let mut cache = pass.lock().await;
        *cache = None;
        let id = run_id.to_string();
        let mut views = self.pool.begin().await.map_err(ProjectError::Database)?;
        for delete in [
            "DELETE FROM petri_stream WHERE run_id = ?",
            "DELETE FROM petri_projection WHERE run_id = ?",
            "DELETE FROM platform_records WHERE run_id = ?",
        ] {
            sqlx::query(delete)
                .bind(&id)
                .execute(&mut *views)
                .await
                .map_err(ProjectError::Database)?;
        }
        views.commit().await.map_err(ProjectError::Database)?;
        let mut records = self.records.begin().await.map_err(ProjectError::Database)?;
        for delete in [
            "DELETE FROM petri_records WHERE run_id = ?",
            "DELETE FROM petri_runs WHERE run_id = ?",
        ] {
            sqlx::query(delete)
                .bind(&id)
                .execute(&mut *records)
                .await
                .map_err(ProjectError::Database)?;
        }
        records.commit().await.map_err(ProjectError::Database)?;
        lock(&self.slots).remove(&run_id);
        Ok(())
    }

    /// The last delivery sequence the run's view holds, or `None` when no
    /// pass has committed a view for it.
    pub async fn stream_head(&self, run_id: RunId) -> Result<Option<u64>, ProjectError> {
        let head: Option<i64> =
            sqlx::query_scalar("SELECT stream_seq FROM petri_projection WHERE run_id = ?")
                .bind(run_id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(ProjectError::Database)?;
        Ok(head.map(|head| u64::try_from(head).unwrap_or(0)))
    }

    /// Schedule a pass for the run. A pass already running for it runs once
    /// more when it ends; any number of signals in between coalesce.
    pub fn signal(self: &Arc<Self>, run_id: RunId) {
        {
            let mut slots = lock(&self.slots);
            let slot = slots.entry(run_id).or_default();
            if slot.running {
                slot.pending = true;
                return;
            }
            slot.running = true;
        }
        let projector = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let again = match projector.project_run(run_id).await {
                    Ok(report) => report.contended,
                    Err(error) => {
                        warn!(
                            run_id = %run_id,
                            error = %collect_chain(&error).join(": "),
                            "Petri projection pass failed; the next signal retries it"
                        );
                        false
                    }
                };
                let mut slots = lock(&projector.slots);
                let slot = slots.entry(run_id).or_default();
                if again || slot.pending {
                    slot.pending = false;
                    continue;
                }
                slot.running = false;
                return;
            }
        });
    }

    /// Wait until no pass is running or pending for the run: a test's way
    /// to observe the view after its signals.
    pub async fn settle(&self, run_id: RunId) {
        loop {
            let idle = {
                let slots = lock(&self.slots);
                slots
                    .get(&run_id)
                    .is_none_or(|slot| !slot.running && !slot.pending)
            };
            if idle {
                return;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Stop the next pass after its reads and before its view transaction,
    /// as a crash there would, once.
    pub fn fail_before_view(&self) {
        self.fault.store(true, Ordering::SeqCst);
    }

    /// One pass over every Petri run the database holds: the runs with a
    /// Petri record, and the runs with platform records. Runs whose view
    /// already covers every committed record are skipped cheaply.
    pub async fn startup_pass(&self) -> Result<StartupReport, ProjectError> {
        let mut ids: Vec<String> = sqlx::query_scalar("SELECT run_id FROM petri_runs")
            .fetch_all(&self.records)
            .await
            .map_err(ProjectError::Database)?;
        let with_platform: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT run_id FROM platform_records")
                .fetch_all(&self.pool)
                .await
                .map_err(ProjectError::Database)?;
        ids.extend(with_platform);
        ids.sort();
        ids.dedup();
        let mut report = StartupReport::default();
        for id in ids {
            let Some(run_id) = projection::run_id_of(&id) else {
                debug!(run_key = %id, "Petri run key is not a Fabro run id; not projected");
                continue;
            };
            report.runs += 1;
            match self.project_run(run_id).await {
                Ok(pass) => {
                    if !pass.skipped {
                        report.projected += 1;
                    }
                }
                // One run's view trailing never stops the server: the next
                // signal for the run retries its pass.
                Err(error) => {
                    warn!(
                        run_id = %run_id,
                        error = %collect_chain(&error).join(": "),
                        "Petri projection pass failed at startup; the next signal retries it"
                    );
                    report.failed += 1;
                }
            }
        }
        if report.projected > 0 {
            info!(
                runs = report.runs,
                projected = report.projected,
                "Petri projections caught up at startup"
            );
        }
        Ok(report)
    }

    /// One view pass for the run. Passes over one run run one at a time.
    pub async fn project_run(&self, run_id: RunId) -> Result<PassReport, ProjectError> {
        self.caches.sweep(IDLE);
        let pass = self.caches.pass_of(run_id);
        let mut slot = pass.lock().await;
        // The view tables are the source of truth: a cache that no longer
        // describes them (another projector committed a pass) is dropped.
        let (positions, stream_seq) = stored_positions(&self.pool, run_id)
            .await?
            .unwrap_or_default();
        let mut run = match slot.take() {
            Some(cache) if cache.matches(&positions, stream_seq) => cache,
            Some(_) => {
                debug!(run_id = %run_id, "the stored view moved under the run's cache; rebuilding it");
                RunCache::over(self.load_view(&run_id).await?)
            }
            None => RunCache::over(self.load_view(&run_id).await?),
        };
        let report = self.pass(run_id, &mut run).await;
        // A finished run's records are complete: its cache is dropped, and
        // the passes its late platform records take rebuild the view whole.
        if !run.view.view.state.finished_run() {
            *slot = Some(run);
        }
        report
    }

    /// The pass over the run's cache: read what is committed past the
    /// positions the cache's view holds, fold it, and write the view.
    async fn pass(&self, run_id: RunId, run: &mut RunCache) -> Result<PassReport, ProjectError> {
        let key = RunKey::new(run_id.to_string());
        let platform_head = self
            .platform
            .head(&run_id)
            .await
            .map_err(ProjectError::Store)?
            .unwrap_or(0);
        let petri_heads = self.petri_heads(&run_id).await?;
        let stored = &run.view;
        let at_head = platform_head == stored.positions.platform_seq
            && petri_heads.iter().all(|(log, head)| {
                stored
                    .positions
                    .petri
                    .iter()
                    .any(|held| log_text(&held.source) == *log && held.seq == *head)
            });
        if at_head && stored.view.projection.is_some() {
            return Ok(PassReport {
                run_id,
                skipped: true,
                contended: false,
                petri_events: 0,
                platform_records: 0,
                replayed_records: 0,
                stream_seq: stored.stream_seq,
                positions: stored.positions.clone(),
                health: stored.view.state.health.clone(),
            });
        }

        let platform_records = self
            .platform
            .read_after(&run_id, stored.positions.platform_seq)
            .await
            .map_err(ProjectError::Store)?;
        let mut replayed_records = 0;
        let (events, replay_failure) = match self.store.open(&key, Access::Read).await {
            Ok(logs) => match run.replay.advance(&*logs).await {
                Ok(new) => {
                    replayed_records = new.iter().filter(|event| event.id.index == 0).count();
                    // A rebuilt replay derives the run whole: only the events
                    // past the view's positions are new to it.
                    let held = run.view.positions.held();
                    let mut events = std::mem::take(&mut run.pending);
                    events.extend(new.into_iter().filter(|event| {
                        held.get(&event.id.source)
                            .is_none_or(|last| event.id > *last)
                    }));
                    (events, None)
                }
                // The replay stood still and is retried by the next pass;
                // what it derived before stays pending.
                Err(error) => {
                    let chain = collect_chain(&error).join(": ");
                    warn!(run_id = %run_id, error = %chain, "Petri run does not replay; the view holds");
                    (Vec::new(), Some(chain))
                }
            },
            Err(StoreError::NotFound { .. }) => (Vec::new(), None),
            Err(error) => return Err(ProjectError::Open(error)),
        };

        let mut view = run.view.view.clone();
        let mut positions = run.view.positions.clone();
        let mut stream_seq = run.view.stream_seq;
        let run_finished = view.state.finished_run()
            || events.iter().any(|event| {
                matches!(
                    event.coordinator(),
                    Some(CoordinatorEvent::RunFinished { .. })
                )
            });
        let platform_head_seen = platform_records
            .last()
            .map_or(positions.platform_seq, |record| record.seq);
        let (items, held) = order_items(
            &events,
            &platform_records,
            &view.state.finished_firings,
            run_finished,
        );
        if held > 0 {
            debug!(
                run_id = %run_id,
                held,
                "platform records held back until their firing's finish is in the stream"
            );
        }

        let mut rows: Vec<StreamRow> = Vec::with_capacity(items.len());
        for item in &items {
            stream_seq += 1;
            view.fold(item, stream_seq);
            let row = match item {
                Item::Petri(event) => {
                    positions.advance(event.id);
                    StreamRow {
                        stream_seq,
                        item_kind: "petri",
                        item_id: event_id_text(&event.id),
                        event_json: serde_json::to_string(event).map_err(ProjectError::Encode)?,
                    }
                }
                Item::Platform(record) => {
                    positions.platform_seq = record.seq;
                    StreamRow {
                        stream_seq,
                        item_kind: "platform",
                        item_id: record.seq.to_string(),
                        event_json: serde_json::to_string(record).map_err(ProjectError::Encode)?,
                    }
                }
            };
            rows.push(row);
        }
        drop(items);
        view.state.health = self.health(&key, &view.state, replay_failure).await?;

        if self.fault.swap(false, Ordering::SeqCst) {
            run.pending = events;
            return Err(ProjectError::Injected);
        }
        let written = self
            .write_view(
                run_id,
                &view,
                &positions,
                stream_seq,
                &rows,
                platform_head_seen,
            )
            .await;
        match written {
            Ok(true) => {}
            Ok(false) => {
                debug!(run_id = %run_id, "platform records landed during the pass; running it again");
                run.pending = events;
                return Ok(PassReport {
                    run_id,
                    skipped: false,
                    contended: true,
                    petri_events: 0,
                    platform_records: 0,
                    replayed_records,
                    stream_seq: 0,
                    positions: Positions::default(),
                    health: RecordHealth::default(),
                });
            }
            Err(error) => {
                run.pending = events;
                return Err(error);
            }
        }
        debug!(
            run_id = %run_id,
            petri_events = events.len(),
            platform_records = platform_records.len(),
            replayed_records,
            stream_seq,
            "Petri projection pass committed"
        );
        let petri_events = events.len();
        let health = view.state.health.clone();
        run.committed(view, positions.clone(), stream_seq);
        if !rows.is_empty() {
            // No receiver is not an error: nobody follows the stream.
            let _ = self.committed.send(run_id);
        }
        Ok(PassReport {
            run_id,
            skipped: false,
            contended: false,
            petri_events,
            platform_records: platform_records.len(),
            replayed_records,
            stream_seq,
            positions,
            health,
        })
    }

    /// The view transaction: the projection row, the stream rows and the
    /// `runs` row, committed together, unless a platform record landed
    /// since the pass read them (`false`: the view is left alone).
    async fn write_view(
        &self,
        run_id: RunId,
        view: &RunView,
        positions: &Positions,
        stream_seq: u64,
        rows: &[StreamRow],
        platform_head_seen: u64,
    ) -> Result<bool, ProjectError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(ProjectError::Database)?;
        let head_now: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) FROM platform_records WHERE run_id = ?",
        )
        .bind(run_id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(ProjectError::Database)?;
        if u64::try_from(head_now).unwrap_or(0) != platform_head_seen {
            drop(tx);
            return Ok(false);
        }
        let projection_json =
            serde_json::to_string(&view.projection).map_err(ProjectError::Encode)?;
        let fold_json = serde_json::to_string(&view.state).map_err(ProjectError::Encode)?;
        let positions_json = serde_json::to_string(positions).map_err(ProjectError::Encode)?;
        sqlx::query(
            "INSERT INTO petri_projection (run_id, projection_json, fold_json, positions_json, \
             stream_seq, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(run_id) DO UPDATE \
             SET projection_json = excluded.projection_json, fold_json = excluded.fold_json, \
             positions_json = excluded.positions_json, stream_seq = excluded.stream_seq, \
             updated_at_ms = excluded.updated_at_ms",
        )
        .bind(run_id.to_string())
        .bind(projection_json)
        .bind(fold_json)
        .bind(positions_json)
        .bind(column(stream_seq))
        .bind(column(now_ms()))
        .execute(&mut *tx)
        .await
        .map_err(ProjectError::Database)?;
        for row in rows {
            sqlx::query(
                "INSERT INTO petri_stream (run_id, stream_seq, item_kind, item_id, event_json) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(run_id.to_string())
            .bind(column(row.stream_seq))
            .bind(row.item_kind)
            .bind(&row.item_id)
            .bind(&row.event_json)
            .execute(&mut *tx)
            .await
            .map_err(ProjectError::Database)?;
        }
        if let Some(projection) = view.projection.as_ref() {
            RunSummaryStore::write_petri_run_row_on_connection(&mut tx, &run_id, projection)
                .await
                .map_err(ProjectError::Store)?;
        }
        tx.commit().await.map_err(ProjectError::Database)?;
        Ok(true)
    }

    /// The stored view of the run, or an empty one.
    async fn load_view(&self, run_id: &RunId) -> Result<StoredView, ProjectError> {
        let row: Option<(String, String, String, i64)> = sqlx::query_as(
            "SELECT projection_json, fold_json, positions_json, stream_seq FROM petri_projection \
             WHERE run_id = ?",
        )
        .bind(run_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(ProjectError::Database)?;
        let Some((projection_json, fold_json, positions_json, stream_seq)) = row else {
            return Ok(StoredView {
                view:       RunView::new(),
                positions:  Positions::default(),
                stream_seq: 0,
            });
        };
        let projection: Option<RunProjection> =
            serde_json::from_str(&projection_json).map_err(ProjectError::Encode)?;
        let state: FoldState = serde_json::from_str(&fold_json).map_err(ProjectError::Encode)?;
        let positions: Positions =
            serde_json::from_str(&positions_json).map_err(ProjectError::Encode)?;
        Ok(StoredView {
            view: RunView { projection, state },
            positions,
            stream_seq: u64::try_from(stream_seq).unwrap_or(0),
        })
    }

    /// The last seq of every Petri log of the run, by the log column's text.
    async fn petri_heads(&self, run_id: &RunId) -> Result<Vec<(String, u64)>, ProjectError> {
        // The coordinator log and the execution logs are what the projection
        // reads; the resources log is the sandbox ledger and has no events.
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT log, MAX(seq) FROM petri_records WHERE run_id = ? AND (log = 'coordinator' \
             OR log LIKE 'execution %') GROUP BY log",
        )
        .bind(run_id.to_string())
        .fetch_all(&self.records)
        .await
        .map_err(ProjectError::Database)?;
        Ok(rows
            .into_iter()
            .map(|(log, seq)| (log, u64::try_from(seq).unwrap_or(0)))
            .collect())
    }

    /// Whether the run's record is whole: a replay failure says no with its
    /// reason; a run that has not recorded its finish is not yet; a finished
    /// run is what `inspect_run` says, checked until it says complete.
    async fn health(
        &self,
        key: &RunKey,
        state: &FoldState,
        replay_failure: Option<String>,
    ) -> Result<RecordHealth, ProjectError> {
        if let Some(failure) = replay_failure {
            return Ok(RecordHealth {
                complete:   false,
                incomplete: vec![failure],
            });
        }
        if state.finished.is_none() {
            return Ok(RecordHealth {
                complete:   false,
                incomplete: vec!["the run has not recorded its finish".to_string()],
            });
        }
        if state.health.complete {
            return Ok(state.health.clone());
        }
        let logs = match self.store.open(key, Access::Read).await {
            Ok(logs) => logs,
            Err(StoreError::NotFound { .. }) => return Ok(state.health.clone()),
            Err(error) => return Err(ProjectError::Open(error)),
        };
        match inspect::inspect_run(&*logs).await {
            Ok(inspection) => Ok(RecordHealth {
                complete:   inspection.complete,
                incomplete: inspection.incomplete,
            }),
            Err(error) => Ok(RecordHealth {
                complete:   false,
                incomplete: vec![collect_chain(&error).join(": ")],
            }),
        }
    }
}

impl Projector {
    /// A run store whose appends signal this projector: for a run that
    /// executes in the same process as the projector, over the SQLite store
    /// directly, where no append endpoint is there to signal. The signal is
    /// sent after the store's append returned, so the records it covers are
    /// durable before the view sees them.
    pub fn observe_store(
        self: &Arc<Self>,
        inner: Arc<dyn petri_execution::RunStore>,
    ) -> Arc<dyn petri_execution::RunStore> {
        Arc::new(SignallingStore {
            inner,
            projector: Arc::clone(self),
        })
    }
}

/// A run store that signals a projector after each append.
struct SignallingStore {
    inner:     Arc<dyn petri_execution::RunStore>,
    projector: Arc<Projector>,
}

#[async_trait::async_trait]
impl petri_execution::RunStore for SignallingStore {
    async fn open(
        &self,
        key: &RunKey,
        access: Access,
    ) -> Result<Arc<dyn petri_execution::RunLogs>, StoreError> {
        let logs = self.inner.open(key, access).await?;
        Ok(Arc::new(SignallingLogs {
            inner:     logs,
            run_id:    projection::run_id_of(key.as_str()),
            projector: Arc::clone(&self.projector),
        }))
    }
}

struct SignallingLogs {
    inner:     Arc<dyn petri_execution::RunLogs>,
    run_id:    Option<RunId>,
    projector: Arc<Projector>,
}

#[async_trait::async_trait]
impl petri_execution::RunLogs for SignallingLogs {
    fn locator(&self) -> String {
        self.inner.locator()
    }

    async fn append(
        &self,
        log: &petri_execution::LogId,
        records: &[petri_execution::Record],
    ) -> Result<(), StoreError> {
        self.inner.append(log, records).await?;
        if let Some(run_id) = self.run_id {
            self.projector.signal(run_id);
        }
        Ok(())
    }

    async fn read(
        &self,
        log: &petri_execution::LogId,
    ) -> Result<Vec<petri_execution::Record>, StoreError> {
        self.inner.read(log).await
    }

    async fn read_from(
        &self,
        log: &petri_execution::LogId,
        seq: u64,
    ) -> Result<Vec<petri_execution::Record>, StoreError> {
        self.inner.read_from(log, seq).await
    }

    async fn put_blob(&self, bytes: &[u8]) -> Result<petri_store::Digest, StoreError> {
        self.inner.put_blob(bytes).await
    }

    async fn get_blob(&self, digest: petri_store::Digest) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_blob(digest).await
    }
}

/// The order one pass streams its new items in, and how many platform
/// records it holds back for a later pass.
///
/// Every item is first ordered by `recorded_at` (stable: the coordinator
/// log before an execution log before a platform record on a tie, and each
/// log's own order kept). A platform record that carries a Petri position
/// (a checkpoint, keyed on `(execution, firing)`) is then placed by that
/// position, not by its clock, because the server stamps the record and the
/// worker stamps Petri's records and the two clocks can tie or invert:
///
/// - before the firing's first `routing.resolved` event in the pass, which is
///   right after the firing's finish (its `step.finished` and the
///   `visit.completed` attached to it) and before the next firing's
///   `visit.started`, which is attached to that routing record;
/// - else after the last event of the firing in the pass;
/// - else, when the firing finished in an earlier pass, before the first event
///   of a later firing (a larger firing id) in the same execution, or where its
///   `recorded_at` put it;
/// - else the record is held back, with every platform record after it, and the
///   pass consumes platform records only up to it. The hook that writes a
///   checkpoint record runs after the driver appended the attempt's finish, but
///   the driver's store writer flushes that record on its own schedule, so the
///   platform record can be committed before its firing's `step.finished`;
///   holding it keeps the stream's order the same live and on a rebuild.
///   Nothing is held once the run has recorded its finish.
///
/// The rule reads only the pass's own items and the firings already
/// finished, so a record is never streamed before its firing's finish and
/// never after the firing's routes.
fn order_items<'a>(
    events: &'a [RunEvent],
    platform_records: &'a [StoredPlatformRecord],
    finished_before: &BTreeSet<String>,
    run_finished: bool,
) -> (Vec<Item<'a>>, usize) {
    let firing_of = |event: &RunEvent| -> Option<(u64, u64)> {
        let execution = event.context.execution?;
        let firing = event.subject.as_ref()?.firing?;
        Some((execution.raw(), firing.raw()))
    };
    let finished_in_pass = |at: (u64, u64)| {
        events.iter().any(|event| {
            firing_of(event) == Some(at)
                && matches!(event.engine(), Some(Event::StepFinished { .. }))
        })
    };
    let finished = |at: (u64, u64)| {
        finished_in_pass(at) || finished_before.contains(&projection::stage_key(at.0, at.1))
    };
    // Platform records are consumed in seq order: the first one whose firing
    // has not finished holds itself and everything after it.
    let consumed = if run_finished {
        platform_records.len()
    } else {
        platform_records
            .iter()
            .position(|record| {
                record
                    .position
                    .is_some_and(|position| !finished((position.execution, position.firing)))
            })
            .unwrap_or(platform_records.len())
    };
    let held = platform_records.len() - consumed;
    let platform_records = &platform_records[..consumed];

    let mut items: Vec<(u64, u8, Item<'a>)> =
        Vec::with_capacity(events.len() + platform_records.len());
    for event in events {
        let rank = match event.id.source {
            EventSource::Coordinator => 0,
            EventSource::Execution { .. } => 1,
        };
        items.push((event.recorded_at, rank, Item::Petri(event)));
    }
    for record in platform_records {
        items.push((record.recorded_at, 2, Item::Platform(record)));
    }
    items.sort_by_key(|(recorded_at, rank, _)| (*recorded_at, *rank));

    let item_firing = |item: &Item<'a>| match item {
        Item::Petri(event) => firing_of(event),
        Item::Platform(_) => None,
    };
    let is_routing = |item: &Item<'a>| {
        matches!(
            item,
            Item::Petri(event) if matches!(event.engine(), Some(Event::RoutingResolved { .. }))
        )
    };
    // The key of each item: its index in clock order, and whether it sits
    // before (0), at (1) or after (2) that index.
    let mut keys: Vec<(usize, u8)> = (0..items.len()).map(|index| (index, 1)).collect();
    for (index, (_, _, item)) in items.iter().enumerate() {
        let Item::Platform(record) = item else {
            continue;
        };
        let Some(position) = record.position else {
            continue;
        };
        let at = (position.execution, position.firing);
        let first_routing = items
            .iter()
            .position(|(_, _, other)| item_firing(other) == Some(at) && is_routing(other));
        let last_of_firing = items
            .iter()
            .rposition(|(_, _, other)| item_firing(other) == Some(at));
        let first_later = items.iter().position(|(_, _, other)| {
            item_firing(other).is_some_and(|(execution, firing)| execution == at.0 && firing > at.1)
        });
        keys[index] = if let Some(before) = first_routing {
            (before, 0)
        } else if let Some(after) = last_of_firing {
            (after, 2)
        } else if let Some(before) = first_later {
            (before, 0)
        } else {
            (index, 1)
        };
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|index| keys[*index]);
    let mut ordered: Vec<Option<Item<'a>>> =
        items.into_iter().map(|(_, _, item)| Some(item)).collect();
    let items = order
        .into_iter()
        .map(|index| ordered[index].take().expect("each item is placed once"))
        .collect();
    (items, held)
}

struct StreamRow {
    stream_seq: u64,
    item_kind:  &'static str,
    item_id:    String,
    event_json: String,
}

/// How many commit signals a slow reader may fall behind before it is told
/// it lagged and re-reads from its cursor.
const COMMIT_SIGNAL_CAPACITY: usize = 1024;

/// The run's stream past the cursor, read from the view tables: up to
/// `limit` rows with `stream_seq > after`, in order, in Fabro's envelope.
pub async fn stream_after(
    views: &DbPool,
    run_id: RunId,
    after: u64,
    limit: usize,
) -> Result<Vec<RunStreamItem>, ProjectError> {
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT stream_seq, item_kind, item_id, event_json FROM petri_stream WHERE run_id = ? AND \
         stream_seq > ? ORDER BY stream_seq LIMIT ?",
    )
    .bind(run_id.to_string())
    .bind(column(after))
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(views)
    .await
    .map_err(ProjectError::Database)?;
    rows.into_iter()
        .map(|(stream_seq, item_kind, item_id, event_json)| {
            let item: serde_json::Value =
                serde_json::from_str(&event_json).map_err(ProjectError::Encode)?;
            let kind = match item_kind.as_str() {
                "platform" => RunStreamItemKind::Platform,
                _ => RunStreamItemKind::Petri,
            };
            let recorded_at = item
                .get("recorded_at")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            Ok(RunStreamItem {
                run_id,
                stream_seq: u64::try_from(stream_seq).unwrap_or(0),
                kind,
                id: item_id,
                recorded_at,
                item,
            })
        })
        .collect()
}

/// A Petri event id as the stream names it: `<log>/<seq>/<index>`.
#[must_use]
pub fn event_id_text(id: &EventId) -> String {
    format!("{}/{}/{}", log_text(&id.source), id.seq, id.index)
}

fn log_text(source: &EventSource) -> String {
    match source {
        EventSource::Coordinator => "coordinator".to_string(),
        EventSource::Execution { execution } => format!("execution {execution}"),
    }
}

fn column(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The run's projection rebuilt from its records alone, with nothing
/// stored: what a fresh projector would commit over the same records. A test
/// compares it with the live view. `records` and `views` are the two pools
/// [`Projector::new`] takes.
pub async fn rebuild(
    records: &DbPool,
    views: &DbPool,
    run_id: RunId,
) -> Result<(Option<RunProjection>, Positions, u64), ProjectError> {
    let store = SqliteRunStore::new(records.clone());
    let platform = PlatformRecordStore::new(views.clone());
    let key = RunKey::new(run_id.to_string());
    let platform_records = platform.read(&run_id).await.map_err(ProjectError::Store)?;
    let events = match store.open(&key, Access::Read).await {
        Ok(logs) => events::replay_run(&*logs)
            .await
            .inspect_err(|error| {
                warn!(error = %collect_chain(error).join(": "), "rebuild: the run does not replay");
            })
            .unwrap_or_default(),
        Err(StoreError::NotFound { .. }) => Vec::new(),
        Err(error) => return Err(ProjectError::Open(error)),
    };
    let run_finished = events.iter().any(|event| {
        matches!(
            event.coordinator(),
            Some(CoordinatorEvent::RunFinished { .. })
        )
    });
    let (items, _held) = order_items(&events, &platform_records, &BTreeSet::new(), run_finished);
    let mut view = RunView::new();
    let mut positions = Positions::default();
    let mut stream_seq = 0;
    for item in &items {
        stream_seq += 1;
        view.fold(item, stream_seq);
        match item {
            Item::Petri(event) => positions.advance(event.id),
            Item::Platform(record) => positions.platform_seq = record.seq,
        }
    }
    Ok((view.projection, positions, stream_seq))
}

/// The stored view's positions and stream sequence, for a test; `views` is
/// the pool the view tables live in.
pub async fn stored_positions(
    views: &DbPool,
    run_id: RunId,
) -> Result<Option<(Positions, u64)>, ProjectError> {
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT positions_json, stream_seq FROM petri_projection WHERE run_id = ?")
            .bind(run_id.to_string())
            .fetch_optional(views)
            .await
            .map_err(ProjectError::Database)?;
    row.map(|(positions, stream_seq)| {
        Ok((
            serde_json::from_str(&positions).map_err(ProjectError::Encode)?,
            u64::try_from(stream_seq).unwrap_or(0),
        ))
    })
    .transpose()
}

/// The stored view's projection, for a test or a reader outside the store.
pub async fn stored_projection(
    views: &DbPool,
    run_id: RunId,
) -> Result<Option<RunProjection>, ProjectError> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT projection_json FROM petri_projection WHERE run_id = ?")
            .bind(run_id.to_string())
            .fetch_optional(views)
            .await
            .map_err(ProjectError::Database)?;
    json.map(|json| serde_json::from_str(&json).map_err(ProjectError::Encode))
        .transpose()
}

/// The stream rows of a run: `(stream_seq, item_kind, item_id)`, in order.
pub async fn stored_stream(
    views: &DbPool,
    run_id: RunId,
) -> Result<Vec<(u64, String, String)>, ProjectError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT stream_seq, item_kind, item_id FROM petri_stream WHERE run_id = ? ORDER BY stream_seq",
    )
    .bind(run_id.to_string())
    .fetch_all(views)
    .await
    .map_err(ProjectError::Database)?;
    Ok(rows
        .into_iter()
        .map(|(seq, kind, id)| (u64::try_from(seq).unwrap_or(0), kind, id))
        .collect())
}

/// Every stored platform record of a run, for a reader outside the store.
pub async fn stored_platform_records(
    views: &DbPool,
    run_id: RunId,
) -> Result<Vec<StoredPlatformRecord>, ProjectError> {
    PlatformRecordStore::new(views.clone())
        .read(&run_id)
        .await
        .map_err(ProjectError::Store)
}

/// A recorded event's projection is what `RunEvent` serializes to.
#[must_use]
pub fn event_json(event: &RunEvent) -> serde_json::Value {
    serde_json::to_value(event).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use fabro_store::PlatformRecord;
    use fabro_store::platform_records::{CheckpointRecord, StagePosition};
    use petri_execution::events::{Context, NodeRef, Record, RecordOrigin, Subject};
    use petri_execution::{ExecutionId, StoredEngineRecord};
    use petri_runtime::driver::BranchRole;
    use petri_runtime::engine::{DecisionId, EventOrigin, RouteApplied};
    use petri_runtime::ir::{Attempt, FiringId, NodeId, Outcome, Status};

    use super::*;

    /// A firing's engine event at `seq`, recorded at `at`.
    fn engine_event(seq: u64, firing: u64, at: u64, body: Event) -> RunEvent {
        RunEvent {
            id:          EventId {
                source: EventSource::Execution {
                    execution: ExecutionId::new(0),
                },
                seq,
                index: 0,
            },
            origin:      RecordOrigin::External,
            context:     Context {
                invocation: None,
                execution:  Some(ExecutionId::new(0)),
                parent:     None,
            },
            subject:     Some(Subject {
                node:       NodeRef {
                    id:   NodeId::new(1),
                    name: format!("n{firing}").into(),
                    kind: "attractor/command".into(),
                    meta: serde_json::Value::Null,
                },
                firing:     Some(FiringId::new(firing)),
                visit:      Some(1),
                attempt:    Some(Attempt::FIRST),
                generation: None,
                branch:     BranchRole::None,
            }),
            observed_at: None,
            recorded_at: at,
            record:      Some(Record::Engine(StoredEngineRecord {
                seq,
                origin: EventOrigin::External,
                recorded_at: at,
                body,
            })),
            derived:     None,
        }
    }

    fn finished(seq: u64, firing: u64, at: u64) -> RunEvent {
        engine_event(seq, firing, at, Event::StepFinished {
            firing:  FiringId::new(firing),
            attempt: Attempt::FIRST,
            outcome: Outcome::new(Status::Success, serde_json::Value::Null),
        })
    }

    fn routing(seq: u64, firing: u64, at: u64) -> RunEvent {
        engine_event(seq, firing, at, Event::RoutingResolved {
            decision_id: DecisionId::route(FiringId::new(firing), Attempt::FIRST),
            groups:      Vec::new(),
        })
    }

    fn applied(seq: u64, firing: u64, at: u64) -> RunEvent {
        engine_event(seq, firing, at, Event::RouteApplied {
            applied: RouteApplied::None {
                firing: FiringId::new(firing),
                group:  0,
            },
        })
    }

    fn started(seq: u64, firing: u64, at: u64) -> RunEvent {
        engine_event(seq, firing, at, Event::StepStarted {
            firing:  FiringId::new(firing),
            attempt: Attempt::FIRST,
        })
    }

    fn checkpoint(seq: u64, firing: u64, at: u64) -> StoredPlatformRecord {
        StoredPlatformRecord {
            seq,
            recorded_at: at,
            record: PlatformRecord::Checkpoint(CheckpointRecord {
                execution: 0,
                firing,
                attempt: Some(1),
                workspace: None,
                git_commit_sha: Some("abc".to_string()),
                diff_summary: None,
                patch_blob: None,
                operation: None,
            }),
            position: Some(StagePosition {
                execution: 0,
                firing,
            }),
        }
    }

    fn names(items: &[Item<'_>]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Petri(event) => event_id_text(&event.id),
                Item::Platform(record) => format!("platform {}", record.seq),
            })
            .collect()
    }

    /// A firing's events, then a later firing's events, then a checkpoint
    /// for the first firing stamped later than all of them: the stream puts
    /// the checkpoint right after the first firing's finish, before its
    /// routes and before the later firing.
    #[test]
    fn a_positioned_record_follows_its_firings_finish_whatever_its_clock_says() {
        let events = vec![
            finished(10, 1, 100),
            routing(11, 1, 101),
            applied(12, 1, 102),
            started(13, 2, 103),
            finished(14, 2, 104),
        ];
        let records = vec![checkpoint(1, 1, 250)];
        let (items, held) = order_items(&events, &records, &BTreeSet::new(), false);
        assert_eq!(held, 0);
        assert_eq!(names(&items), vec![
            "execution 0/10/0",
            "platform 1",
            "execution 0/11/0",
            "execution 0/12/0",
            "execution 0/13/0",
            "execution 0/14/0",
        ]);
    }

    /// With the firing finished in an earlier pass, the record goes before
    /// the first event of a later firing; a record with no position keeps
    /// its clock order.
    #[test]
    fn a_positioned_record_precedes_later_firings_and_an_unpositioned_one_keeps_its_clock() {
        let events = vec![started(13, 2, 103), finished(14, 2, 104)];
        let records = vec![checkpoint(1, 1, 250)];
        let finished_before: BTreeSet<String> = [projection::stage_key(0, 1)].into_iter().collect();
        let (items, held) = order_items(&events, &records, &finished_before, false);
        assert_eq!(held, 0);
        assert_eq!(names(&items), vec![
            "platform 1",
            "execution 0/13/0",
            "execution 0/14/0",
        ]);

        let unpositioned = StoredPlatformRecord {
            position: None,
            ..checkpoint(2, 1, 250)
        };
        let unpositioned = [unpositioned];
        let (items, held) = order_items(&events, &unpositioned, &BTreeSet::new(), false);
        assert_eq!(held, 0);
        assert_eq!(names(&items), vec![
            "execution 0/13/0",
            "execution 0/14/0",
            "platform 2",
        ]);
    }

    /// A record whose firing has no finish yet, in the stream or in the pass,
    /// is held back with everything after it until the finish arrives, or
    /// until the run has finished.
    #[test]
    fn a_positioned_record_is_held_until_its_firings_finish_is_in_the_stream() {
        let events = vec![started(13, 2, 103), finished(14, 2, 104)];
        let records = vec![
            checkpoint(1, 2, 50),
            checkpoint(2, 3, 60),
            checkpoint(3, 2, 70),
        ];
        let (items, held) = order_items(&events, &records, &BTreeSet::new(), false);
        assert_eq!(
            held, 2,
            "the record for firing 3 holds itself and the one after it"
        );
        assert_eq!(names(&items), vec![
            "execution 0/13/0",
            "execution 0/14/0",
            "platform 1",
        ]);

        // Once the run finished, a record for a firing that never finished
        // keeps its clock order; the firing's own records still follow its
        // finish, in their seq order.
        let (items, held) = order_items(&events, &records, &BTreeSet::new(), true);
        assert_eq!(held, 0, "nothing is held once the run finished");
        assert_eq!(names(&items), vec![
            "platform 2",
            "execution 0/13/0",
            "execution 0/14/0",
            "platform 1",
            "platform 3",
        ]);
    }
}
