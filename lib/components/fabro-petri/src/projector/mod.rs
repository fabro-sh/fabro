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
pub(crate) mod order;
mod signalling;
pub(crate) mod stream;

use std::collections::{BTreeMap, HashMap};
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fabro_db::DbPool;
use fabro_store::platform_records::{PlatformRecordStore, now_ms};
use fabro_store::{RunProjection, RunSummaryStore};
use fabro_types::{GitHubHost, RunId, RunStreamItem};
use fabro_util::error::collect_chain;
use fabro_util::sync;
use petri_execution::events::{EventId, EventSource, RunEvent};
use petri_execution::{Access, CoordinatorEvent, RunKey, RunStore as _, inspect};
use petri_store::StoreError;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time;
use tracing::{debug, info, warn};

use self::cache::{Caches, IDLE, RunCache};
use self::stream::StreamRow;
use crate::SqliteRunStore;
use crate::projection::{self, FoldState, RecordHealth, RunView};

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

impl PassReport {
    /// A pass that found the view at the head of every log and wrote
    /// nothing.
    fn skipped(run_id: RunId, stored: &StoredView) -> Self {
        Self {
            run_id,
            skipped: true,
            contended: false,
            petri_events: 0,
            platform_records: 0,
            replayed_records: 0,
            stream_seq: stored.stream_seq,
            positions: stored.positions.clone(),
            health: stored.view.state.health.clone(),
        }
    }

    /// A pass that left the view alone because a platform record landed
    /// under it; the projector runs it again.
    fn contended(run_id: RunId, replayed_records: usize) -> Self {
        Self {
            run_id,
            skipped: false,
            contended: true,
            petri_events: 0,
            platform_records: 0,
            replayed_records,
            stream_seq: 0,
            positions: Positions::default(),
            health: RecordHealth::default(),
        }
    }
}

/// What a pass read past its cache: the events new to the view, the
/// replay failure that held it, and how many records the replay cost.
struct NewEvents {
    events:           Vec<RunEvent>,
    replay_failure:   Option<String>,
    replayed_records: usize,
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
    /// A test's fault: stop the next pass after its reads, before its
    /// view transaction, as a crash there would.
    #[cfg(any(test, feature = "test-support"))]
    fault:             AtomicBool,
    /// Sent after each committed pass that wrote stream rows: the run whose
    /// stream grew. A wake-up for the stream's readers, never a source of
    /// facts; a reader that lags re-reads from its cursor.
    committed:         broadcast::Sender<RunId>,
    /// The GitHub deployment run origins are classified against.
    github_host:       GitHubHost,
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
        Self::with_github_host(records, views, GitHubHost::default())
    }

    /// [`Projector::new`] classifying run origins against `github_host`.
    #[must_use]
    pub fn with_github_host(records: DbPool, views: DbPool, github_host: GitHubHost) -> Arc<Self> {
        Arc::new(Self {
            github_host,
            store: SqliteRunStore::new(records.clone()),
            platform: PlatformRecordStore::new(views.clone()),
            records,
            pool: views,
            slots: Mutex::default(),
            caches: Caches::default(),
            #[cfg(any(test, feature = "test-support"))]
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
        stream::stream_after(&self.pool, run_id, after, limit).await
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
        sync::lock(&self.slots).remove(&run_id);
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
            let mut slots = sync::lock(&self.slots);
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
                let mut slots = sync::lock(&projector.slots);
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
                let slots = sync::lock(&self.slots);
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_before_view(&self) {
        self.fault.store(true, Ordering::SeqCst);
    }

    /// Whether a test asked this pass to stop before its view transaction;
    /// never outside tests.
    fn take_fault(&self) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.fault.swap(false, Ordering::SeqCst)
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            false
        }
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
        if self.at_head(run_id, &run.view).await? {
            return Ok(PassReport::skipped(run_id, &run.view));
        }

        let platform_records = self
            .platform
            .read_after(&run_id, run.view.positions.platform_seq)
            .await
            .map_err(ProjectError::Store)?;
        let NewEvents {
            events,
            replay_failure,
            replayed_records,
        } = self.read_new_events(run_id, &key, run).await?;

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
        let (items, held) = order::order_items(
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
        let rows = stream::stream_rows(&items, &mut view, &mut positions, &mut stream_seq)?;
        drop(items);
        view.state.health = self.health(&key, &view.state, replay_failure).await?;

        if self.take_fault() {
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
                return Ok(PassReport::contended(run_id, replayed_records));
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

    /// Whether the stored view already covers every committed record: the
    /// platform head and every Petri log's head are the positions it holds,
    /// and it has a projection to serve.
    async fn at_head(&self, run_id: RunId, stored: &StoredView) -> Result<bool, ProjectError> {
        let platform_head = self
            .platform
            .head(&run_id)
            .await
            .map_err(ProjectError::Store)?
            .unwrap_or(0);
        let petri_heads = self.petri_heads(&run_id).await?;
        Ok(platform_head == stored.positions.platform_seq
            && petri_heads.iter().all(|(log, head)| {
                stored
                    .positions
                    .petri
                    .iter()
                    .any(|held| stream::log_text(&held.source) == *log && held.seq == *head)
            })
            && stored.view.projection.is_some())
    }

    /// The events past the view's positions: the cache's replay advanced
    /// over the records committed since, led by the events an earlier pass
    /// derived and did not commit. A rebuilt replay derives the run whole,
    /// so only the events past the view's positions are new to it. A
    /// replay that fails (a torn tail) stands still, yields nothing, and
    /// names its reason; what it derived before stays pending for the next
    /// pass.
    async fn read_new_events(
        &self,
        run_id: RunId,
        key: &RunKey,
        run: &mut RunCache,
    ) -> Result<NewEvents, ProjectError> {
        let nothing = NewEvents {
            events:           Vec::new(),
            replay_failure:   None,
            replayed_records: 0,
        };
        let logs = match self.store.open(key, Access::Read).await {
            Ok(logs) => logs,
            Err(StoreError::NotFound { .. }) => return Ok(nothing),
            Err(error) => return Err(ProjectError::Open(error)),
        };
        match run.replay.advance(&*logs).await {
            Ok(new) => {
                let replayed_records = new.iter().filter(|event| event.id.index == 0).count();
                let held = run.view.positions.held();
                let mut events = std::mem::take(&mut run.pending);
                events.extend(new.into_iter().filter(|event| {
                    held.get(&event.id.source)
                        .is_none_or(|last| event.id > *last)
                }));
                Ok(NewEvents {
                    events,
                    replay_failure: None,
                    replayed_records,
                })
            }
            Err(error) => {
                let chain = collect_chain(&error).join(": ");
                warn!(run_id = %run_id, error = %chain, "Petri run does not replay; the view holds");
                Ok(NewEvents {
                    replay_failure: Some(chain),
                    ..nothing
                })
            }
        }
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
        .bind(stream::column(stream_seq))
        .bind(stream::column(now_ms()))
        .execute(&mut *tx)
        .await
        .map_err(ProjectError::Database)?;
        for row in rows {
            sqlx::query(
                "INSERT INTO petri_stream (run_id, stream_seq, item_kind, item_id, event_json) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(run_id.to_string())
            .bind(stream::column(row.stream_seq))
            .bind(row.item_kind)
            .bind(&row.item_id)
            .bind(&row.event_json)
            .execute(&mut *tx)
            .await
            .map_err(ProjectError::Database)?;
        }
        if let Some(projection) = view.projection.as_ref() {
            RunSummaryStore::write_petri_run_row_on_connection(
                &mut tx,
                &self.github_host,
                &run_id,
                projection,
            )
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

/// How many commit signals a slow reader may fall behind before it is told
/// it lagged and re-reads from its cursor.
const COMMIT_SIGNAL_CAPACITY: usize = 1024;

/// The stored view's positions and stream sequence; `views` is the pool the
/// view tables live in.
pub(crate) async fn stored_positions(
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
