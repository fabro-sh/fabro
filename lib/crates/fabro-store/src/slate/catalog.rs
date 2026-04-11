use chrono::{Datelike, Timelike};
use fabro_types::RunId;
use slatedb::Db;

use crate::{ListRunsQuery, Result, keys};

pub(crate) async fn write_index(db: &Db, run_id: &RunId) -> Result<()> {
    db.put(keys::runs_index_by_start_key(run_id), []).await?;
    Ok(())
}

pub(crate) async fn delete_index(db: &Db, run_id: &RunId) -> Result<()> {
    db.delete(keys::runs_index_by_start_key(run_id)).await?;
    Ok(())
}

pub(crate) async fn list_run_ids(db: &Db, query: &ListRunsQuery) -> Result<Vec<RunId>> {
    let mut iter = db.scan_prefix(keys::runs_index_by_start_prefix()).await?;
    let mut run_ids = Vec::new();
    while let Some(entry) = iter.next().await? {
        let key = String::from_utf8(entry.key.to_vec()).map_err(|err| {
            crate::StoreError::Other(format!("stored key is not valid UTF-8: {err}"))
        })?;
        let Some(run_id) = keys::parse_run_id_from_index_key(&key) else {
            continue;
        };
        let created_at = run_id.created_at();
        if let Some(start) = query.start {
            if created_at < start {
                continue;
            }
        }
        if let Some(end) = query.end {
            if created_at > end {
                continue;
            }
        }
        run_ids.push(run_id);
    }
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
