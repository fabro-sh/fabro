use std::sync::Arc;

use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::path::Path;

use crate::{ListRunsQuery, Result};
use fabro_types::RunId;

pub(crate) async fn write_catalog(
    store: Arc<dyn ObjectStore>,
    base_prefix: &str,
    run_id: &RunId,
) -> Result<()> {
    store
        .put(&by_id_path(base_prefix, run_id), bytes::Bytes::new().into())
        .await?;
    store
        .put(
            &by_start_path(base_prefix, run_id),
            bytes::Bytes::new().into(),
        )
        .await?;
    Ok(())
}

pub(crate) async fn read_locator(
    store: Arc<dyn ObjectStore>,
    base_prefix: &str,
    run_id: &RunId,
) -> Result<bool> {
    match store.head(&by_id_path(base_prefix, run_id)).await {
        Ok(_) => Ok(true),
        Err(object_store::Error::NotFound { .. }) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

pub(crate) async fn list_run_ids(
    store: Arc<dyn ObjectStore>,
    base_prefix: &str,
    query: &ListRunsQuery,
) -> Result<Vec<RunId>> {
    let prefix = Path::from(format!("{base_prefix}by-start"));
    let metas = store.list(Some(&prefix)).try_collect::<Vec<_>>().await?;
    let mut run_ids = Vec::new();
    for meta in metas {
        let Some(run_id) = parse_run_id_from_path(&meta.location) else {
            continue;
        };
        if !read_locator(store.clone(), base_prefix, &run_id).await? {
            continue;
        }
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
    Ok(run_ids)
}

pub(crate) fn db_prefix(base_prefix: &str, run_id: &RunId) -> String {
    format!(
        "{base_prefix}db/{}/{run_id}/",
        run_id.created_at().format("%Y-%m-%d-%H-%M-%S-%3f")
    )
}

pub(crate) fn by_id_path(base_prefix: &str, run_id: &RunId) -> Path {
    Path::from(format!("{base_prefix}by-id/{run_id}.json"))
}

pub(crate) fn by_start_path(base_prefix: &str, run_id: &RunId) -> Path {
    Path::from(format!(
        "{base_prefix}by-start/{}/{run_id}.json",
        run_id.created_at().format("%Y-%m-%d-%H-%M")
    ))
}

pub(crate) fn parse_run_id_from_path(path: &Path) -> Option<RunId> {
    let filename = path.filename()?;
    let run_id = filename.strip_suffix(".json").unwrap_or(filename);
    run_id.parse().ok()
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::collections::HashSet;

    pub(crate) async fn repair_catalog(
        store: Arc<dyn ObjectStore>,
        base_prefix: &str,
    ) -> Result<()> {
        let by_id_prefix = Path::from(format!("{base_prefix}by-id"));
        let by_start_prefix = Path::from(format!("{base_prefix}by-start"));

        let by_id_metas = store
            .list(Some(&by_id_prefix))
            .try_collect::<Vec<_>>()
            .await?;
        let mut canonical = HashSet::new();
        for meta in by_id_metas {
            if let Some(run_id) = parse_run_id_from_path(&meta.location) {
                canonical.insert(run_id);
            }
        }

        for run_id in &canonical {
            let path = by_start_path(base_prefix, run_id);
            if !object_exists(store.clone(), &path).await? {
                store.put(&path, bytes::Bytes::new().into()).await?;
            }
        }

        let by_start_metas = store
            .list(Some(&by_start_prefix))
            .try_collect::<Vec<_>>()
            .await?;
        let mut seen = HashSet::new();
        for meta in by_start_metas {
            let location = meta.location.clone();
            let Some(run_id) = parse_run_id_from_path(&location) else {
                delete_if_exists(store.clone(), &location).await?;
                continue;
            };
            if !canonical.contains(&run_id) {
                delete_if_exists(store.clone(), &location).await?;
                continue;
            }
            let expected = by_start_path(base_prefix, &run_id);
            if expected == location {
                seen.insert(run_id);
            } else {
                delete_if_exists(store.clone(), &location).await?;
            }
        }

        for run_id in &canonical {
            if !seen.contains(run_id) {
                store
                    .put(
                        &by_start_path(base_prefix, run_id),
                        bytes::Bytes::new().into(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn object_exists(store: Arc<dyn ObjectStore>, path: &Path) -> Result<bool> {
        match store.head(path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    async fn delete_if_exists(store: Arc<dyn ObjectStore>, path: &Path) -> Result<()> {
        match store.delete(path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}
