use std::sync::Arc;

use chrono::{Datelike, Timelike};
use fabro_types::RunId;
use futures::TryStreamExt;

use crate::record::{MarkerCodec, Record, Repository};
use crate::{ListRunsQuery, Result};

#[derive(Debug, Default)]
pub(crate) struct RunCatalogEntry;

impl Record for RunCatalogEntry {
    type Id = RunId;
    type Codec = MarkerCodec;

    const PREFIX: &'static str = "runs/_index/by-start";

    fn id(&self) -> Self::Id {
        unreachable!("marker records must use put_at")
    }
}

pub struct RunCatalogIndex {
    repo: Repository<RunCatalogEntry>,
}

impl std::fmt::Debug for RunCatalogIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCatalogIndex").finish_non_exhaustive()
    }
}

impl RunCatalogIndex {
    pub(crate) fn new(db: Arc<slatedb::Db>) -> Self {
        Self {
            repo: Repository::new(db),
        }
    }

    pub async fn add(&self, run_id: &RunId) -> Result<()> {
        self.repo.put_at(run_id, &RunCatalogEntry).await
    }

    pub async fn remove(&self, run_id: &RunId) -> Result<()> {
        self.repo.delete(run_id).await
    }

    pub async fn list(&self, query: &ListRunsQuery) -> Result<Vec<RunId>> {
        let mut run_ids = self.repo.scan_ids_stream().try_collect::<Vec<_>>().await?;
        run_ids.retain(|run_id| {
            let created_at = run_id.created_at();
            if let Some(start) = query.start {
                if created_at < start {
                    return false;
                }
            }
            if let Some(end) = query.end {
                if created_at > end {
                    return false;
                }
            }
            true
        });
        run_ids.sort_by_key(|run_id| {
            let created_at = run_id.created_at();
            (
                created_at.year(),
                created_at.month(),
                created_at.day(),
                created_at.hour(),
                created_at.minute(),
                *run_id,
            )
        });
        Ok(run_ids)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration as ChronoDuration, TimeZone, Utc};
    use object_store::memory::InMemory;
    use ulid::Ulid;

    use super::RunCatalogIndex;
    use crate::ListRunsQuery;

    async fn index() -> RunCatalogIndex {
        let db = Arc::new(
            slatedb::Db::open("run-catalog-index-tests", Arc::new(InMemory::new()))
                .await
                .unwrap(),
        );
        RunCatalogIndex::new(db)
    }

    #[tokio::test]
    async fn add_list_and_remove_round_trip() {
        let index = index().await;
        let early = fabro_types::RunId::from(Ulid::from_datetime(
            Utc.with_ymd_and_hms(2026, 4, 20, 9, 0, 0).unwrap().into(),
        ));
        let later = fabro_types::RunId::from(Ulid::from_datetime(
            Utc.with_ymd_and_hms(2026, 4, 20, 9, 1, 0).unwrap().into(),
        ));

        index.add(&later).await.unwrap();
        index.add(&early).await.unwrap();

        assert_eq!(index.list(&ListRunsQuery::default()).await.unwrap(), vec![
            early, later
        ]);

        index.remove(&early).await.unwrap();
        assert_eq!(index.list(&ListRunsQuery::default()).await.unwrap(), vec![
            later
        ]);
    }

    #[tokio::test]
    async fn list_applies_start_and_end_filters() {
        let index = index().await;
        let first = fabro_types::RunId::from(Ulid::from_datetime(
            Utc.with_ymd_and_hms(2026, 4, 20, 9, 0, 0).unwrap().into(),
        ));
        let second = fabro_types::RunId::from(Ulid::from_datetime(
            Utc.with_ymd_and_hms(2026, 4, 20, 9, 1, 0).unwrap().into(),
        ));
        let third = fabro_types::RunId::from(Ulid::from_datetime(
            Utc.with_ymd_and_hms(2026, 4, 20, 9, 2, 0).unwrap().into(),
        ));
        for run_id in [first, second, third] {
            index.add(&run_id).await.unwrap();
        }

        assert_eq!(
            index
                .list(&ListRunsQuery {
                    start: Some(second.created_at()),
                    end:   Some(second.created_at() + ChronoDuration::seconds(1)),
                })
                .await
                .unwrap(),
            vec![second]
        );
    }
}
