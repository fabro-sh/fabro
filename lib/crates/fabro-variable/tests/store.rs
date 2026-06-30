use fabro_variable::{Error, ImportStatus, VariableStore, import_legacy_json_once};
use tokio::fs;

struct TestStore {
    dir:   tempfile::TempDir,
    pool:  fabro_db::DbPool,
    store: VariableStore,
}

async fn test_store() -> anyhow::Result<TestStore> {
    let dir = tempfile::tempdir()?;
    let pool = fabro_db::connect(dir.path().join("fabro.sqlite3")).await?;
    fabro_db::migrate(&pool).await?;
    let store = VariableStore::new(pool.clone());
    Ok(TestStore { dir, pool, store })
}

#[tokio::test]
async fn new_store_lists_empty_database() -> anyhow::Result<()> {
    let test = test_store().await?;

    assert!(test.store.list().await?.is_empty());

    Ok(())
}

#[tokio::test]
async fn set_get_list_and_reopen_variables() -> anyhow::Result<()> {
    let test = test_store().await?;

    let first = test
        .store
        .set("ZETA", "last", Some("Last variable"))
        .await?;
    let second = test.store.set("ALPHA", "", None).await?;

    assert_eq!(first.name, "ZETA");
    assert_eq!(first.value, "last");
    assert_eq!(first.description.as_deref(), Some("Last variable"));
    assert_eq!(second.value, "");
    assert_eq!(test.store.get("ZETA").await?.unwrap().value, "last");
    assert_eq!(
        test.store
            .list()
            .await?
            .into_iter()
            .map(|variable| variable.name)
            .collect::<Vec<_>>(),
        vec!["ALPHA", "ZETA"]
    );

    let reopened = VariableStore::new(test.pool.clone());
    assert_eq!(reopened.get("ALPHA").await?.unwrap().value, "");
    assert_eq!(
        reopened.get("ZETA").await?.unwrap().description.as_deref(),
        Some("Last variable")
    );

    Ok(())
}

#[tokio::test]
async fn upsert_preserves_description_when_omitted_and_updates_when_present() -> anyhow::Result<()>
{
    let test = test_store().await?;

    let created = test
        .store
        .set("DEPLOY_ENV", "staging", Some("Deployment target"))
        .await?;
    let preserved = test.store.set("DEPLOY_ENV", "production", None).await?;
    let updated = test
        .store
        .set("DEPLOY_ENV", "preview", Some("Preview target"))
        .await?;

    assert_eq!(preserved.created_at, created.created_at);
    assert_eq!(preserved.description.as_deref(), Some("Deployment target"));
    assert_eq!(updated.description.as_deref(), Some("Preview target"));
    assert!(updated.updated_at >= preserved.updated_at);

    Ok(())
}

#[tokio::test]
async fn update_existing_preserves_description_and_reports_missing() -> anyhow::Result<()> {
    let test = test_store().await?;

    let created = test
        .store
        .set("DEPLOY_ENV", "staging", Some("Deployment target"))
        .await?;
    let updated = test
        .store
        .update_existing("DEPLOY_ENV", "production", None)
        .await?;

    assert_eq!(updated.created_at, created.created_at);
    assert_eq!(updated.value, "production");
    assert_eq!(updated.description.as_deref(), Some("Deployment target"));
    assert!(matches!(
        test.store.update_existing("MISSING", "value", None).await,
        Err(Error::NotFound(name)) if name == "MISSING"
    ));

    Ok(())
}

#[tokio::test]
async fn remove_deletes_variable_and_reports_missing() -> anyhow::Result<()> {
    let test = test_store().await?;
    test.store.set("DEPLOY_ENV", "staging", None).await?;

    test.store.remove("DEPLOY_ENV").await?;

    assert!(test.store.get("DEPLOY_ENV").await?.is_none());
    assert!(matches!(
        test.store.remove("DEPLOY_ENV").await,
        Err(Error::NotFound(name)) if name == "DEPLOY_ENV"
    ));

    Ok(())
}

#[tokio::test]
async fn env_style_names_are_required() -> anyhow::Result<()> {
    let test = test_store().await?;

    for invalid in ["", "1BAD", "bad-name", "BAD.NAME"] {
        assert!(matches!(
            test.store.set(invalid, "value", None).await,
            Err(Error::InvalidName(name)) if name == invalid
        ));
    }

    test.store.set("_OK", "value", None).await?;
    test.store.set("OK_123", "value", None).await?;

    Ok(())
}

#[tokio::test]
async fn value_map_snapshots_values_only() -> anyhow::Result<()> {
    let test = test_store().await?;
    test.store
        .set("DEPLOY_ENV", "staging", Some("Deployment target"))
        .await?;

    let values = test.store.value_map().await?;

    assert_eq!(
        values.get("DEPLOY_ENV").map(String::as_str),
        Some("staging")
    );

    Ok(())
}

#[tokio::test]
async fn import_missing_legacy_file_is_noop_without_marker() -> anyhow::Result<()> {
    let test = test_store().await?;
    let source = test.dir.path().join("variables.json");

    let report = import_legacy_json_once(&test.pool, &source).await?;

    assert!(report.is_none());
    assert!(test.store.list().await?.is_empty());
    assert_eq!(legacy_import_count(&test.pool).await?, 0);

    Ok(())
}

#[tokio::test]
async fn import_valid_legacy_json_imports_rows_and_marker() -> anyhow::Result<()> {
    let test = test_store().await?;
    let source = test.dir.path().join("variables.json");
    fs::write(
        &source,
        r#"{
  "ALPHA": {
    "value": "first",
    "description": "Alpha variable",
    "created_at": "2026-06-30T10:00:00Z",
    "updated_at": "2026-06-30T10:01:00Z"
  },
  "EMPTY": {
    "value": "",
    "created_at": "2026-06-30T10:02:00Z",
    "updated_at": "2026-06-30T10:03:00Z"
  }
}"#,
    )
    .await?;

    let report = import_legacy_json_once(&test.pool, &source)
        .await?
        .expect("existing legacy file should report import");

    assert_eq!(report.status, ImportStatus::Imported);
    assert_eq!(report.imported_rows, 2);
    assert_eq!(report.skipped_rows, 0);
    assert_eq!(report.variable_names, vec!["ALPHA", "EMPTY"]);
    assert_eq!(test.store.get("ALPHA").await?.unwrap().value, "first");
    assert_eq!(
        test.store
            .get("ALPHA")
            .await?
            .unwrap()
            .description
            .as_deref(),
        Some("Alpha variable")
    );
    assert_eq!(test.store.get("EMPTY").await?.unwrap().value, "");
    assert_eq!(
        legacy_import_status(&test.pool).await?,
        Some("imported".to_string())
    );

    Ok(())
}

#[tokio::test]
async fn import_empty_legacy_json_records_zero_row_marker() -> anyhow::Result<()> {
    let test = test_store().await?;
    let source = test.dir.path().join("variables.json");
    fs::write(&source, "{}").await?;

    let report = import_legacy_json_once(&test.pool, &source)
        .await?
        .expect("existing empty legacy file should report import");

    assert_eq!(report.status, ImportStatus::Imported);
    assert_eq!(report.imported_rows, 0);
    assert_eq!(report.skipped_rows, 0);
    assert!(report.variable_names.is_empty());
    assert_eq!(
        legacy_import_status(&test.pool).await?,
        Some("imported".to_string())
    );

    Ok(())
}

#[tokio::test]
async fn import_second_run_is_noop_after_marker() -> anyhow::Result<()> {
    let test = test_store().await?;
    let source = test.dir.path().join("variables.json");
    fs::write(
        &source,
        r#"{
  "ALPHA": {
    "value": "first",
    "created_at": "2026-06-30T10:00:00Z",
    "updated_at": "2026-06-30T10:01:00Z"
  }
}"#,
    )
    .await?;

    let first = import_legacy_json_once(&test.pool, &source).await?;
    let second = import_legacy_json_once(&test.pool, &source).await?;

    assert!(first.is_some());
    assert!(second.is_none());
    assert_eq!(test.store.list().await?.len(), 1);

    Ok(())
}

#[tokio::test]
async fn import_skips_legacy_file_when_sqlite_already_has_rows() -> anyhow::Result<()> {
    let test = test_store().await?;
    test.store.set("CURRENT", "sqlite wins", None).await?;
    let source = test.dir.path().join("variables.json");
    fs::write(
        &source,
        r#"{
  "LEGACY": {
    "value": "file loses",
    "created_at": "2026-06-30T10:00:00Z",
    "updated_at": "2026-06-30T10:01:00Z"
  }
}"#,
    )
    .await?;

    let report = import_legacy_json_once(&test.pool, &source)
        .await?
        .expect("existing SQLite rows should report skipped import");

    assert_eq!(report.status, ImportStatus::SkippedExistingTarget);
    assert_eq!(report.imported_rows, 0);
    assert_eq!(report.skipped_rows, 1);
    assert_eq!(report.variable_names, vec!["CURRENT"]);
    assert!(test.store.get("LEGACY").await?.is_none());
    assert_eq!(
        legacy_import_status(&test.pool).await?,
        Some("skipped_existing_target".to_string())
    );

    Ok(())
}

#[tokio::test]
async fn import_invalid_json_or_name_fails_when_sqlite_is_empty() -> anyhow::Result<()> {
    let invalid_json = test_store().await?;
    let invalid_json_source = invalid_json.dir.path().join("variables.json");
    fs::write(&invalid_json_source, "not json").await?;

    let error = import_legacy_json_once(&invalid_json.pool, &invalid_json_source)
        .await
        .expect_err("invalid JSON should fail import");
    assert!(
        error
            .to_string()
            .contains(&invalid_json_source.display().to_string())
    );

    let invalid_name = test_store().await?;
    let invalid_name_source = invalid_name.dir.path().join("variables.json");
    fs::write(
        &invalid_name_source,
        r#"{
  "BAD-NAME": {
    "value": "secret-ish value must not be in errors",
    "created_at": "2026-06-30T10:00:00Z",
    "updated_at": "2026-06-30T10:01:00Z"
  }
}"#,
    )
    .await?;

    let error = import_legacy_json_once(&invalid_name.pool, &invalid_name_source)
        .await
        .expect_err("invalid name should fail import");
    let message = error.to_string();
    assert!(message.contains(&invalid_name_source.display().to_string()));
    assert!(message.contains("BAD-NAME"));
    assert!(!message.contains("secret-ish value"));

    Ok(())
}

async fn legacy_import_count(pool: &fabro_db::DbPool) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM legacy_imports")
        .fetch_one(pool)
        .await?)
}

async fn legacy_import_status(pool: &fabro_db::DbPool) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT status FROM legacy_imports WHERE import_name = 'variables-json-v1'",
    )
    .fetch_optional(pool)
    .await?)
}
