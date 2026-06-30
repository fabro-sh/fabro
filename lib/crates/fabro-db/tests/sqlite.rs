use sqlx::Row as _;

#[tokio::test]
async fn connect_creates_parent_directory_and_migrate_is_idempotent() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("nested").join("fabro.sqlite3");

    let database = fabro_db::Database::connect(&db_path).await?;
    database.migrate().await?;
    database.migrate().await?;
    database.health_check().await?;

    assert!(db_path.exists());
    let variable_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'variables'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(variable_table_count, 1);

    let environments_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'environments'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(environments_table_count, 1);

    let legacy_import_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'legacy_imports'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(legacy_import_table_count, 0);

    let foreign_keys: i64 = sqlx::query("PRAGMA foreign_keys")
        .fetch_one(database.pool())
        .await?
        .get(0);
    assert_eq!(foreign_keys, 1);

    Ok(())
}

#[tokio::test]
async fn environments_schema_rejects_invalid_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    insert_minimal_environment(database.pool(), "valid", "docker", "allow_all").await?;

    for (id, provider, network_mode) in [
        ("Bad", "docker", "allow_all"),
        ("local", "docker", "allow_all"),
        ("bad-provider", "bogus", "allow_all"),
        ("bad-network", "docker", "bogus"),
    ] {
        let result = insert_minimal_environment(database.pool(), id, provider, network_mode).await;
        assert!(
            result.is_err(),
            "environment row should be rejected: id={id}, provider={provider}, network_mode={network_mode}"
        );
    }

    Ok(())
}

async fn insert_minimal_environment(
    pool: &fabro_db::DbPool,
    id: &str,
    provider: &str,
    network_mode: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        INSERT INTO environments (
            id,
            revision,
            provider,
            network_mode,
            lifecycle_preserve,
            lifecycle_stop_on_terminal
        )
        VALUES (?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(id)
    .bind("a".repeat(64))
    .bind(provider)
    .bind(network_mode)
    .bind(false)
    .bind(true)
    .execute(pool)
    .await?;

    Ok(())
}

#[tokio::test]
async fn variables_schema_enforces_env_style_names() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    sqlx::query("INSERT INTO variables (name, value, created_at, updated_at) VALUES (?, ?, ?, ?)")
        .bind("OK_123")
        .bind("")
        .bind("2026-06-30T00:00:00Z")
        .bind("2026-06-30T00:00:00Z")
        .execute(database.pool())
        .await?;

    let invalid = sqlx::query(
        "INSERT INTO variables (name, value, created_at, updated_at) VALUES (?, ?, ?, ?)",
    )
    .bind("1BAD")
    .bind("value")
    .bind("2026-06-30T00:00:00Z")
    .bind("2026-06-30T00:00:00Z")
    .execute(database.pool())
    .await;
    assert!(invalid.is_err());

    Ok(())
}
