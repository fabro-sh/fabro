use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::path::Path;

use crate::{CatalogRecord, ListRunsQuery, Result};

pub(crate) async fn write_catalog(
    store: Arc<dyn ObjectStore>,
    base_prefix: &str,
    run_id: &str,
    created_at: DateTime<Utc>,
    db_prefix: &str,
    run_dir: Option<&str>,
) -> Result<CatalogRecord> {
    let record = CatalogRecord {
        run_id: run_id.to_string(),
        created_at,
        db_prefix: db_prefix.to_string(),
        run_dir: run_dir.map(ToOwned::to_owned),
    };
    let bytes = serde_json::to_vec(&record)?;
    store
        .put(&by_id_path(base_prefix, run_id), bytes.clone().into())
        .await?;
    store
        .put(
            &by_start_path(base_prefix, created_at, run_id),
            bytes.into(),
        )
        .await?;
    Ok(record)
}

pub(crate) async fn read_locator(
    store: Arc<dyn ObjectStore>,
    base_prefix: &str,
    run_id: &str,
) -> Result<Option<CatalogRecord>> {
    read_catalog_path(store, by_id_path(base_prefix, run_id)).await
}

pub(crate) async fn list_catalogs(
    store: Arc<dyn ObjectStore>,
    base_prefix: &str,
    query: &ListRunsQuery,
) -> Result<Vec<CatalogRecord>> {
    let prefix = Path::from(format!("{base_prefix}by-start"));
    let metas = store.list(Some(&prefix)).try_collect::<Vec<_>>().await?;
    let mut records = Vec::new();
    for meta in metas {
        let Some(record) = read_catalog_path(store.clone(), meta.location).await? else {
            continue;
        };
        if let Some(start) = query.start {
            if record.created_at < start {
                continue;
            }
        }
        if let Some(end) = query.end {
            if record.created_at > end {
                continue;
            }
        }
        records.push(record);
    }
    Ok(records)
}

pub(super) async fn repair_catalog(store: Arc<dyn ObjectStore>, base_prefix: &str) -> Result<()> {
    let by_id_prefix = Path::from(format!("{base_prefix}by-id"));
    let by_start_prefix = Path::from(format!("{base_prefix}by-start"));

    let by_id_metas = store
        .list(Some(&by_id_prefix))
        .try_collect::<Vec<_>>()
        .await?;
    let mut canonical = HashMap::new();
    for meta in by_id_metas {
        if let Some(record) = read_catalog_path(store.clone(), meta.location).await? {
            canonical.insert(record.run_id.clone(), record);
        }
    }

    for record in canonical.values() {
        let path = by_start_path(base_prefix, record.created_at, &record.run_id);
        if !object_exists(store.clone(), &path).await? {
            store.put(&path, serde_json::to_vec(record)?.into()).await?;
        }
    }

    let by_start_metas = store
        .list(Some(&by_start_prefix))
        .try_collect::<Vec<_>>()
        .await?;
    let mut seen = HashSet::new();
    for meta in by_start_metas {
        let location = meta.location.clone();
        let Some(record) = read_catalog_path(store.clone(), location.clone()).await? else {
            delete_if_exists(store.clone(), &location).await?;
            continue;
        };
        let expected = canonical.get(&record.run_id).map(|canonical_record| {
            by_start_path(base_prefix, canonical_record.created_at, &record.run_id)
        });
        match expected {
            Some(expected) if expected == location => {
                seen.insert(record.run_id);
            }
            _ => {
                delete_if_exists(store.clone(), &location).await?;
            }
        }
    }

    for record in canonical.values() {
        if !seen.contains(&record.run_id) {
            store
                .put(
                    &by_start_path(base_prefix, record.created_at, &record.run_id),
                    serde_json::to_vec(record)?.into(),
                )
                .await?;
        }
    }
    Ok(())
}

pub(crate) fn db_prefix(base_prefix: &str, created_at: DateTime<Utc>, run_id: &str) -> String {
    format!(
        "{base_prefix}db/{}/{run_id}/",
        created_at.format("%Y-%m-%d-%H-%M-%S-%3f")
    )
}

pub(crate) fn by_id_path(base_prefix: &str, run_id: &str) -> Path {
    Path::from(format!("{base_prefix}by-id/{run_id}.json"))
}

pub(crate) fn by_start_path(base_prefix: &str, created_at: DateTime<Utc>, run_id: &str) -> Path {
    Path::from(format!(
        "{base_prefix}by-start/{}/{run_id}.json",
        created_at.format("%Y-%m-%d-%H-%M")
    ))
}

pub(crate) async fn read_catalog_path(
    store: Arc<dyn ObjectStore>,
    path: Path,
) -> Result<Option<CatalogRecord>> {
    match store.get(&path).await {
        Ok(result) => Ok(Some(serde_json::from_slice(&result.bytes().await?)?)),
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(err) => Err(err.into()),
    }
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
