//! The state a live run's passes continue from, kept in memory between
//! passes: Petri's replay of the run (the coordinator state, each
//! execution's engine state, the projection) and Fabro's view as the last
//! committed pass left it. With it a pass reads and folds only the records
//! past the ones the view holds, so its cost is the new records', not the
//! run's.
//!
//! The cache is never a source of facts. It is dropped when the run
//! records its finish, when it has not been used for [`IDLE`], when the
//! stored view moves under it (another projector committed a pass), when
//! the run is deleted, and with the process; the first pass after that
//! rebuilds it by a full replay, which is what every pass did before the
//! cache existed. A replay that fails (a torn tail) stands still and is
//! retried by the next pass. Every pass, cached or not, commits the same
//! rows: the rebuild test in `tests/projection.rs` compares the two.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fabro_types::RunId;
use petri_execution::events::{RunEvent, RunReplay};
use tokio::sync::Mutex as AsyncMutex;

use super::{Positions, StoredView};
use crate::projection::RunView;

/// How long a run's cache is kept after its last pass. A run blocked on a
/// question for longer pays one full replay when its next record lands.
pub(super) const IDLE: Duration = Duration::from_mins(10);

/// One live run's cache.
pub(super) struct RunCache {
    pub(super) replay:  RunReplay,
    /// Events derived by an earlier pass that committed nothing: a pass
    /// that found a platform record landing under it, or that failed
    /// before its view transaction. They lead the next pass's events.
    pub(super) pending: Vec<RunEvent>,
    /// The view as the last committed pass left it, with its positions.
    pub(super) view:    StoredView,
}

impl RunCache {
    /// A cache over the stored view, with a replay that has consumed
    /// nothing: the first advance replays the run whole.
    pub(super) fn over(view: StoredView) -> Self {
        Self {
            replay: RunReplay::new(),
            pending: Vec::new(),
            view,
        }
    }

    /// Whether the cache still describes the stored view: its positions and
    /// delivery sequence are the ones the view tables hold.
    pub(super) fn matches(&self, positions: &Positions, stream_seq: u64) -> bool {
        self.view.stream_seq == stream_seq
            && self.view.positions.platform_seq == positions.platform_seq
            && self.view.positions.held() == positions.held()
    }

    /// The pass committed: the view moved on, and nothing is pending.
    pub(super) fn committed(&mut self, view: RunView, positions: Positions, stream_seq: u64) {
        self.view = StoredView {
            view,
            positions,
            stream_seq,
        };
        self.pending.clear();
    }
}

/// The caches of every run the projector passed over, each behind the
/// run's pass lock, so a pass and a sweep never race over one cache.
#[derive(Default)]
pub(crate) struct Caches {
    runs: Mutex<HashMap<RunId, Entry>>,
}

struct Entry {
    pass:    Arc<AsyncMutex<Option<RunCache>>>,
    touched: Instant,
}

impl Caches {
    /// The run's pass lock, holding its cache if one is kept; the run counts
    /// as used now.
    pub(super) fn pass_of(&self, run_id: RunId) -> Arc<AsyncMutex<Option<RunCache>>> {
        let mut runs = lock(&self.runs);
        let entry = runs.entry(run_id).or_insert_with(|| Entry {
            pass:    Arc::default(),
            touched: Instant::now(),
        });
        entry.touched = Instant::now();
        Arc::clone(&entry.pass)
    }

    /// Drop the cache of every run not used for `idle`, and forget the runs
    /// with no cache and no pass under way. A run whose pass is running is
    /// in use and left alone. How many caches were dropped.
    pub(crate) fn sweep(&self, idle: Duration) -> usize {
        let mut runs = lock(&self.runs);
        let mut dropped = 0;
        runs.retain(|_, entry| {
            if entry.touched.elapsed() < idle {
                return true;
            }
            let Ok(mut cache) = entry.pass.try_lock() else {
                return true;
            };
            if cache.take().is_some() {
                dropped += 1;
            }
            drop(cache);
            // An `Arc` held elsewhere is a pass about to take the lock: the
            // entry stays so the run keeps one lock.
            Arc::strong_count(&entry.pass) > 1
        });
        dropped
    }

    /// Whether a cache is kept for the run: a test's view of the cache.
    pub(crate) fn holds(&self, run_id: RunId) -> bool {
        let runs = lock(&self.runs);
        runs.get(&run_id)
            .is_some_and(|entry| entry.pass.try_lock().is_ok_and(|cache| cache.is_some()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
