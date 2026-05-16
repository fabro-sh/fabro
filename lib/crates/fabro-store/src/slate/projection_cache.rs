use std::collections::HashMap;
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
    entries: Mutex<HashMap<RunId, CachedRunProjection>>,
}

impl RunProjectionCache {
    pub(crate) async fn replace_all(&self, entries: Vec<CachedRunProjection>) {
        let mut cache = self.entries.lock().await;
        cache.clear();
        cache.extend(entries.into_iter().map(|entry| (entry.run_id, entry)));
    }

    pub(crate) async fn replace(&self, entry: CachedRunProjection) {
        self.entries.lock().await.insert(entry.run_id, entry);
    }

    pub(crate) async fn list(&self, query: &ListRunsQuery) -> Vec<CachedRunProjection> {
        let cache = self.entries.lock().await;
        let mut entries = cache
            .values()
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
            .cloned()
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
        self.entries.lock().await.get(run_id).cloned()
    }

    pub(crate) async fn get_summary(&self, run_id: &RunId) -> Option<Run> {
        self.entries
            .lock()
            .await
            .get(run_id)
            .map(|entry| entry.summary.clone())
    }

    pub(crate) async fn apply_event(&self, run_id: &RunId, event: &EventEnvelope) -> Result<()> {
        let mut cache = self.entries.lock().await;
        let Some(entry) = cache.get(run_id) else {
            if event.seq == 1 {
                let projection = RunProjection::apply_events(std::slice::from_ref(event))?;
                cache.insert(
                    *run_id,
                    CachedRunProjection::from_projection(*run_id, projection, event.seq),
                );
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
        cache.insert(
            *run_id,
            CachedRunProjection::from_projection(*run_id, projection, event.seq),
        );
        Ok(())
    }

    pub(crate) async fn remove(&self, run_id: &RunId) {
        self.entries.lock().await.remove(run_id);
    }
}
