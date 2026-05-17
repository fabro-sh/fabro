use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use fabro_types::{Run, RunId, RunProjection};
use tokio::sync::Mutex;

use crate::run_state::{RunProjectionReducer, build_summary};
use crate::{Error, EventEnvelope, ListRunsQuery, Result};

#[derive(Debug, Clone)]
pub struct CachedRunProjection {
    pub run_id:     RunId,
    pub summary:    Run,
    pub projection: Arc<RunProjection>,
    pub last_seq:   u32,
}

impl CachedRunProjection {
    pub(crate) fn from_projection(run_id: RunId, projection: RunProjection, last_seq: u32) -> Self {
        let summary = build_summary(&projection, &run_id);
        Self {
            run_id,
            summary,
            projection: Arc::new(projection),
            last_seq,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct RunProjectionCache {
    state: Mutex<RunProjectionCacheState>,
}

#[derive(Debug, Default)]
struct RunProjectionCacheState {
    entries:            HashMap<RunId, CachedRunProjection>,
    children_by_parent: HashMap<RunId, BTreeSet<RunId>>,
}

impl RunProjectionCacheState {
    fn replace_all(&mut self, entries: Vec<CachedRunProjection>) {
        self.entries.clear();
        self.children_by_parent.clear();
        for entry in entries {
            self.insert(entry);
        }
    }

    fn insert(&mut self, entry: CachedRunProjection) {
        let run_id = entry.run_id;
        let parent_id = entry.summary.parent_id;
        if let Some(previous) = self.entries.insert(run_id, entry) {
            self.remove_parent_index(&previous);
        }
        if let Some(parent_id) = parent_id {
            self.children_by_parent
                .entry(parent_id)
                .or_default()
                .insert(run_id);
        }
    }

    fn remove(&mut self, run_id: &RunId) {
        if let Some(entry) = self.entries.remove(run_id) {
            self.remove_parent_index(&entry);
        }
    }

    fn remove_parent_index(&mut self, entry: &CachedRunProjection) {
        let Some(parent_id) = entry.summary.parent_id else {
            return;
        };
        let Some(children) = self.children_by_parent.get_mut(&parent_id) else {
            return;
        };
        children.remove(&entry.run_id);
        if children.is_empty() {
            self.children_by_parent.remove(&parent_id);
        }
    }

    fn count_children(&self, run_id: &RunId) -> u64 {
        self.children_by_parent
            .get(run_id)
            .map_or(0, |children| children.len() as u64)
    }

    fn with_children_count(&self, mut entry: CachedRunProjection) -> CachedRunProjection {
        entry.summary.children_count = self.count_children(&entry.run_id);
        entry
    }
}

impl RunProjectionCache {
    pub(crate) async fn replace_all(&self, entries: Vec<CachedRunProjection>) {
        self.state.lock().await.replace_all(entries);
    }

    pub(crate) async fn replace(&self, entry: CachedRunProjection) {
        self.state.lock().await.insert(entry);
    }

    pub(crate) async fn list(&self, query: &ListRunsQuery) -> Vec<CachedRunProjection> {
        let entries = {
            let state = self.state.lock().await;
            let raw = match query.parent_id {
                Some(parent_id) => state
                    .children_by_parent
                    .get(&parent_id)
                    .into_iter()
                    .flat_map(|children| children.iter())
                    .filter_map(|run_id| state.entries.get(run_id).cloned())
                    .collect::<Vec<_>>(),
                None => state.entries.values().cloned().collect::<Vec<_>>(),
            };
            raw.into_iter()
                .map(|entry| state.with_children_count(entry))
                .collect::<Vec<_>>()
        };
        let mut entries = entries
            .into_iter()
            .filter(|entry| {
                let created_at = entry.run_id.created_at();
                if query.start.is_some_and(|start| created_at < start) {
                    return false;
                }
                if query.end.is_some_and(|end| created_at > end) {
                    return false;
                }
                true
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .run_id
                .created_at()
                .cmp(&left.run_id.created_at())
                .then_with(|| right.run_id.cmp(&left.run_id))
        });
        entries
    }

    pub(crate) async fn get(&self, run_id: &RunId) -> Option<CachedRunProjection> {
        let state = self.state.lock().await;
        state
            .entries
            .get(run_id)
            .cloned()
            .map(|entry| state.with_children_count(entry))
    }

    pub(crate) async fn get_summary(&self, run_id: &RunId) -> Option<Run> {
        let state = self.state.lock().await;
        state.entries.get(run_id).map(|entry| {
            let mut summary = entry.summary.clone();
            summary.children_count = state.count_children(run_id);
            summary
        })
    }

    pub(crate) async fn apply_event(&self, run_id: &RunId, event: &EventEnvelope) -> Result<()> {
        let mut state = self.state.lock().await;
        let Some(entry) = state.entries.get(run_id).cloned() else {
            if event.seq == 1 {
                let projection = RunProjection::apply_events(std::slice::from_ref(event))?;
                state.insert(CachedRunProjection::from_projection(
                    *run_id, projection, event.seq,
                ));
            } else {
                return Err(Error::InvalidEvent(format!(
                    "projection cache cannot initialize run {run_id} from event seq {}",
                    event.seq
                )));
            }
            return Ok(());
        };

        if event.seq <= entry.last_seq {
            return Ok(());
        }
        if event.seq != entry.last_seq.saturating_add(1) {
            return Err(Error::Other(format!(
                "projection cache sequence gap for run {run_id}: last_seq={}, event_seq={}",
                entry.last_seq, event.seq
            )));
        }

        let mut projection = (*entry.projection).clone();
        projection.apply_event(event)?;
        state.insert(CachedRunProjection::from_projection(
            *run_id, projection, event.seq,
        ));
        Ok(())
    }

    pub(crate) async fn remove(&self, run_id: &RunId) {
        self.state.lock().await.remove(run_id);
    }
}
