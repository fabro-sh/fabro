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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        for path in [
            db_path.clone(),
            db_path.with_extension("sqlite3-wal"),
            db_path.with_extension("sqlite3-shm"),
        ] {
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600,
                "{} should be private",
                path.display()
            );
        }
    }
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

    let secrets_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'secrets'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(secrets_table_count, 1);

    let mcp_servers_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'mcp_servers'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(mcp_servers_table_count, 1);

    for table in ["automations", "automation_triggers"] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(count, 1, "{table} table should exist");
    }

    let runs_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'runs'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(runs_table_count, 1);

    let blobs_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'blobs'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(blobs_table_count, 1);

    for table in [
        "auth_sessions",
        "refresh_tokens",
        "oauth_authorization_codes",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(count, 1, "{table} table should exist");
    }

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
async fn blobs_schema_enforces_canonical_hashes_and_required_data() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    let columns = sqlx::query("PRAGMA table_info(blobs)")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(columns.len(), 2);

    assert_eq!(columns[0].get::<String, _>("name"), "hash");
    assert_eq!(columns[0].get::<String, _>("type"), "TEXT");
    assert_eq!(columns[0].get::<i64, _>("notnull"), 1);
    assert_eq!(columns[0].get::<i64, _>("pk"), 1);
    assert_eq!(columns[0].get::<Option<String>, _>("dflt_value"), None);

    assert_eq!(columns[1].get::<String, _>("name"), "data");
    assert_eq!(columns[1].get::<String, _>("type"), "BLOB");
    assert_eq!(columns[1].get::<i64, _>("notnull"), 1);
    assert_eq!(columns[1].get::<i64, _>("pk"), 0);
    assert_eq!(columns[1].get::<Option<String>, _>("dflt_value"), None);

    let binary_hash = "0".repeat(64);
    let binary_data = vec![0, 0xff, 0x80, b'a'];
    sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
        .bind(&binary_hash)
        .bind(&binary_data)
        .execute(database.pool())
        .await?;
    let stored_binary: Vec<u8> = sqlx::query_scalar("SELECT data FROM blobs WHERE hash = ?")
        .bind(&binary_hash)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(stored_binary, binary_data);

    let empty_hash = "1".repeat(64);
    sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
        .bind(&empty_hash)
        .bind(Vec::<u8>::new())
        .execute(database.pool())
        .await?;
    let stored_empty: Vec<u8> = sqlx::query_scalar("SELECT data FROM blobs WHERE hash = ?")
        .bind(&empty_hash)
        .fetch_one(database.pool())
        .await?;
    assert!(stored_empty.is_empty());

    for invalid_hash in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        let result = sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
            .bind(&invalid_hash)
            .bind(Vec::<u8>::new())
            .execute(database.pool())
            .await;
        assert!(
            result.is_err(),
            "invalid blob hash should be rejected: {invalid_hash:?}"
        );
    }

    let null_hash = sqlx::query("INSERT INTO blobs (hash, data) VALUES (NULL, ?)")
        .bind(Vec::<u8>::new())
        .execute(database.pool())
        .await;
    assert!(null_hash.is_err());

    let null_data = sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, NULL)")
        .bind("2".repeat(64))
        .execute(database.pool())
        .await;
    assert!(null_data.is_err());

    let duplicate_hash = sqlx::query("INSERT INTO blobs (hash, data) VALUES (?, ?)")
        .bind(&binary_hash)
        .bind(vec![1_u8])
        .execute(database.pool())
        .await;
    assert!(duplicate_hash.is_err());

    Ok(())
}

#[tokio::test]
async fn mcp_servers_schema_rejects_invalid_transport_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    insert_mcp_server(
        database.pool(),
        "stdio",
        "stdio",
        None,
        Some(r#"["server"]"#),
        None,
        None,
        Some("{}"),
        None,
    )
    .await?;
    insert_mcp_server(
        database.pool(),
        "http",
        "http",
        Some("streamable_http"),
        None,
        Some("https://example.com/mcp"),
        None,
        None,
        Some("{}"),
    )
    .await?;
    insert_mcp_server(
        database.pool(),
        "sandbox",
        "sandbox",
        Some("sse"),
        Some(r#"["server"]"#),
        None,
        Some(3000),
        Some("{}"),
        None,
    )
    .await?;

    for result in [
        insert_mcp_server(
            database.pool(),
            "bad-id_",
            "stdio",
            None,
            Some(r#"["server"]"#),
            None,
            None,
            Some("{}"),
            None,
        )
        .await,
        insert_mcp_server(
            database.pool(),
            "empty-command",
            "stdio",
            None,
            Some("[]"),
            None,
            None,
            Some("{}"),
            None,
        )
        .await,
        insert_mcp_server(
            database.pool(),
            "http-with-env",
            "http",
            Some("streamable_http"),
            None,
            Some("https://example.com/mcp"),
            None,
            Some("{}"),
            Some("{}"),
        )
        .await,
        insert_mcp_server(
            database.pool(),
            "sandbox-port",
            "sandbox",
            Some("streamable_http"),
            Some(r#"["server"]"#),
            None,
            Some(65_536),
            Some("{}"),
            None,
        )
        .await,
    ] {
        assert!(result.is_err(), "invalid MCP server row should be rejected");
    }

    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "schema test helper mirrors the mutually exclusive transport columns"
)]
async fn insert_mcp_server(
    pool: &fabro_db::DbPool,
    id: &str,
    transport_type: &str,
    protocol: Option<&str>,
    command_json: Option<&str>,
    url: Option<&str>,
    port: Option<i64>,
    env_json: Option<&str>,
    headers_json: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        INSERT INTO mcp_servers (
            id,
            revision,
            display_name,
            transport_type,
            protocol,
            command_json,
            url,
            port,
            env_json,
            headers_json,
            startup_timeout_secs,
            tool_timeout_secs
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(id)
    .bind("a".repeat(64))
    .bind("MCP Server")
    .bind(transport_type)
    .bind(protocol)
    .bind(command_json)
    .bind(url)
    .bind(port)
    .bind(env_json)
    .bind(headers_json)
    .bind(10_i64)
    .bind(60_i64)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn automations_schema_enforces_aggregate_constraints() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    insert_minimal_automation(database.pool(), "valid", 1).await?;
    sqlx::query(
        "INSERT INTO automation_triggers (automation_id, id, enabled, expression) \
         VALUES (?, ?, ?, ?)",
    )
    .bind("valid")
    .bind("nightly")
    .bind(true)
    .bind("0 3 * * *")
    .execute(database.pool())
    .await?;

    assert!(
        insert_minimal_automation(database.pool(), "Bad", 1)
            .await
            .is_err()
    );
    assert!(
        insert_minimal_automation(database.pool(), "bad-bool", 2_i64)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO automation_triggers (automation_id, id, enabled, expression) \
             VALUES (?, ?, ?, ?)",
        )
        .bind("valid")
        .bind("Bad!")
        .bind(true)
        .bind("0 4 * * *")
        .execute(database.pool())
        .await
        .is_err()
    );
    for (repository, branch, tag, sha) in [
        (Some("fabro-sh/workflows"), None, None, None),
        (None, Some("main"), None, None),
        (None, None, Some("v1"), None),
        (
            Some("fabro-sh/workflows"),
            Some("main"),
            None,
            Some("short"),
        ),
    ] {
        let result = sqlx::query(
            "UPDATE automations SET workflow_source_repository = ?, \
             workflow_source_branch = ?, workflow_source_tag = ?, workflow_source_sha = ? \
             WHERE id = 'valid'",
        )
        .bind(repository)
        .bind(branch)
        .bind(tag)
        .bind(sha)
        .execute(database.pool())
        .await;
        assert!(
            result.is_err(),
            "invalid workflow source row should be rejected"
        );
    }

    sqlx::query(
        "UPDATE automations SET workflow_source_repository = 'fabro-sh/workflows', \
         workflow_source_branch = 'main', workflow_source_tag = 'v1', \
         workflow_source_sha = '0123456789abcdef0123456789abcdef01234567' WHERE id = 'valid'",
    )
    .execute(database.pool())
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO automation_triggers (automation_id, id, enabled, expression) \
             VALUES (?, ?, ?, ?)",
        )
        .bind("missing")
        .bind("nightly")
        .bind(true)
        .bind("0 4 * * *")
        .execute(database.pool())
        .await
        .is_err()
    );

    insert_minimal_environment(database.pool(), "automation-env", "docker", "allow_all").await?;
    sqlx::query("UPDATE automations SET environment_id = 'automation-env' WHERE id = 'valid'")
        .execute(database.pool())
        .await?;
    assert!(
        sqlx::query("DELETE FROM environments WHERE id = 'automation-env'")
            .execute(database.pool())
            .await
            .is_err(),
        "an environment referenced by an automation must be protected by a foreign key"
    );
    sqlx::query("UPDATE automations SET environment_id = NULL WHERE id = 'valid'")
        .execute(database.pool())
        .await?;

    sqlx::query("DELETE FROM automations WHERE id = ?")
        .bind("valid")
        .execute(database.pool())
        .await?;
    let trigger_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM automation_triggers WHERE automation_id = ?")
            .bind("valid")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(trigger_count, 0);

    Ok(())
}

#[tokio::test]
async fn automation_workflow_sources_migrate_without_rewriting_existing_rows() -> anyhow::Result<()>
{
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fabro.sqlite3");
    let database = fabro_db::Database::connect(&db_path).await?;
    database.migrate().await?;
    rewind_automation_workflow_source_migration(&database).await?;

    insert_minimal_automation(database.pool(), "preserved", 1).await?;
    sqlx::query(
        "INSERT INTO automation_triggers (automation_id, id, enabled, expression) \
         VALUES ('preserved', 'nightly', 1, '0 3 * * *')",
    )
    .execute(database.pool())
    .await?;

    database.migrate().await?;

    let row = sqlx::query(
        "SELECT id, revision, target_repository, target_branch, target_tag, target_sha, \
         target_workflow, workflow_source_repository, workflow_source_branch, \
         workflow_source_tag, workflow_source_sha \
         FROM automations WHERE id = 'preserved'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(row.get::<String, _>("id"), "preserved");
    assert_eq!(row.get::<String, _>("revision"), "a".repeat(64));
    assert_eq!(row.get::<String, _>("target_repository"), "fabro-sh/fabro");
    assert_eq!(row.get::<String, _>("target_branch"), "main");
    assert_eq!(row.get::<Option<String>, _>("target_tag"), None);
    assert_eq!(row.get::<Option<String>, _>("target_sha"), None);
    assert_eq!(row.get::<String, _>("target_workflow"), "release");
    assert_eq!(
        row.get::<Option<String>, _>("workflow_source_repository"),
        None
    );
    assert_eq!(row.get::<Option<String>, _>("workflow_source_branch"), None);
    assert_eq!(row.get::<Option<String>, _>("workflow_source_tag"), None);
    assert_eq!(row.get::<Option<String>, _>("workflow_source_sha"), None);
    let trigger_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM automation_triggers WHERE automation_id = 'preserved'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(trigger_count, 1);
    assert!(fabro_db::pre_migration_snapshot_path(&db_path).exists());

    database.migrate().await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM automations WHERE id = 'preserved'")
            .fetch_one(database.pool())
            .await?,
        1
    );
    Ok(())
}

async fn rewind_automation_workflow_source_migration(
    database: &fabro_db::Database,
) -> anyhow::Result<()> {
    sqlx::query("DROP TRIGGER automation_workflow_source_all_or_none_update")
        .execute(database.pool())
        .await?;
    sqlx::query("DROP TRIGGER automation_workflow_source_all_or_none_insert")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN workflow_source_sha")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN workflow_source_tag")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN workflow_source_branch")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN workflow_source_repository")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 2026082803")
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn insert_minimal_automation(
    pool: &fabro_db::DbPool,
    id: &str,
    api_enabled: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        INSERT INTO automations (
            id,
            revision,
            name,
            api_enabled,
            target_repository,
            target_branch,
            target_tag,
            target_sha,
            target_workflow
        ) VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, ?)
        ",
    )
    .bind(id)
    .bind("a".repeat(64))
    .bind("Automation")
    .bind(api_enabled)
    .bind("fabro-sh/fabro")
    .bind("main")
    .bind("release")
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn automation_targets_migrate_offline_and_preserve_related_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fabro.sqlite3");
    let database = fabro_db::Database::connect(&db_path).await?;
    database.migrate().await?;
    rewind_automation_target_migration(&database).await?;

    let values = [
        ("sha", "ABCDEF0123456789ABCDEF0123456789ABCDEF01"),
        ("tag-ref", "refs/tags/v1.2.3"),
        ("tag", "tags/v2"),
        ("head-ref", "refs/heads/release"),
        ("head", "heads/feature/test"),
        ("head-literal", "HEAD"),
        ("branch", "feature/bare"),
    ];
    for (id, selector) in values {
        insert_legacy_automation(database.pool(), id, selector).await?;
    }
    sqlx::query(
        "INSERT INTO automation_triggers (automation_id, id, enabled, expression) \
         VALUES ('tag-ref', 'nightly', 1, '0 3 * * *')",
    )
    .execute(database.pool())
    .await?;

    database.migrate().await?;

    let rows = sqlx::query(
        "SELECT id, revision, target_branch, target_tag, target_sha, target_workflow \
         FROM automations ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    let projected = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("id"),
                row.get::<String, _>("target_branch"),
                row.get::<Option<String>, _>("target_tag"),
                row.get::<Option<String>, _>("target_sha"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(projected, vec![
        ("branch".to_string(), "feature/bare".to_string(), None, None),
        ("head".to_string(), "feature/test".to_string(), None, None),
        ("head-literal".to_string(), "main".to_string(), None, None),
        ("head-ref".to_string(), "release".to_string(), None, None),
        (
            "sha".to_string(),
            "main".to_string(),
            None,
            Some("abcdef0123456789abcdef0123456789abcdef01".to_string()),
        ),
        (
            "tag".to_string(),
            "main".to_string(),
            Some("v2".to_string()),
            None
        ),
        (
            "tag-ref".to_string(),
            "main".to_string(),
            Some("v1.2.3".to_string()),
            None,
        ),
    ]);
    assert!(
        rows.iter()
            .all(|row| row.get::<String, _>("revision") == "a".repeat(64))
    );
    assert!(
        rows.iter()
            .all(|row| row.get::<String, _>("target_workflow") == "release")
    );
    let trigger_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM automation_triggers WHERE automation_id = 'tag-ref'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(trigger_count, 1);
    assert!(fabro_db::pre_migration_snapshot_path(&db_path).exists());

    database.migrate().await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM automations")
            .fetch_one(database.pool())
            .await?,
        7
    );
    Ok(())
}

#[tokio::test]
async fn unsupported_automation_targets_abort_before_schema_changes() -> anyhow::Result<()> {
    for selector in ["refs/pull/123/head", "refs/heads/-bad", "tags/HEAD"] {
        let dir = tempfile::tempdir()?;
        let db_path = dir.path().join("fabro.sqlite3");
        let database = fabro_db::Database::connect(&db_path).await?;
        database.migrate().await?;
        rewind_automation_target_migration(&database).await?;
        insert_legacy_automation(database.pool(), "blocked", selector).await?;

        let error = database.migrate().await.expect_err("migration must abort");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("edit it to a branch"), "{rendered}");
        let columns = sqlx::query("PRAGMA table_info(automations)")
            .fetch_all(database.pool())
            .await?;
        let names = columns
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "target_ref"));
        assert!(!names.iter().any(|name| name == "target_branch"));
        let stored: String =
            sqlx::query_scalar("SELECT target_ref FROM automations WHERE id = 'blocked'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(stored, selector);
        assert!(fabro_db::pre_migration_snapshot_path(&db_path).exists());
    }
    Ok(())
}

async fn rewind_automation_target_migration(database: &fabro_db::Database) -> anyhow::Result<()> {
    sqlx::query("DROP INDEX automations_environment_id_idx")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN last_error")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN environment_id")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN target_sha")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations DROP COLUMN target_tag")
        .execute(database.pool())
        .await?;
    sqlx::query("ALTER TABLE automations RENAME COLUMN target_branch TO target_ref")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version IN (2026082601, 2026082801)")
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn insert_legacy_automation(
    pool: &fabro_db::DbPool,
    id: &str,
    selector: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO automations (\
            id, revision, name, api_enabled, target_repository, target_ref, target_workflow\
         ) VALUES (?, ?, 'Automation', 1, 'fabro-sh/fabro', ?, 'release')",
    )
    .bind(id)
    .bind("a".repeat(64))
    .bind(selector)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn runs_schema_creates_indexes_and_rejects_invalid_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    let index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'runs_by_%'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(index_count, 5);

    insert_minimal_run(database.pool(), "submitted", 0, r#"{"id":"run"}"#).await?;
    for (status, diff_additions, summary_json) in [
        ("unknown", 0, r#"{"id":"run-2"}"#),
        ("submitted", -1, r#"{"id":"run-3"}"#),
        ("submitted", 0, "not-json"),
    ] {
        assert!(
            insert_minimal_run(database.pool(), status, diff_additions, summary_json)
                .await
                .is_err()
        );
    }

    Ok(())
}

/// The `runs` row is the projection's summary of a Petri run: the columns
/// the list views filter and sort by, and the JSON the API serves. The
/// legacy event log's tables and the columns only it wrote are gone.
#[tokio::test]
async fn runs_schema_has_its_final_shape_without_the_legacy_event_log() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    for table in [
        "run_events",
        "legacy_run_history_activation",
        "legacy_run_history_deletions",
        "runs_next",
    ] {
        assert!(
            !table_exists(database.pool(), table).await?,
            "{table} must not exist"
        );
    }

    let run_columns = sqlx::query("PRAGMA table_info(runs)")
        .fetch_all(database.pool())
        .await?
        .iter()
        .map(|column| column.get::<String, _>("name"))
        .collect::<Vec<_>>();
    assert_eq!(run_columns, [
        "id",
        "created_at_ms",
        "started_at_ms",
        "last_event_at_ms",
        "completed_at_ms",
        "status",
        "archived_at_ms",
        "parent_id",
        "title",
        "workflow_slug",
        "workflow_name",
        "repository_name",
        "automation_id",
        "diff_additions",
        "diff_deletions",
        "total_usd_micros",
        "summary_json",
    ]);

    let index_names = sqlx::query("PRAGMA index_list(runs)")
        .fetch_all(database.pool())
        .await?
        .iter()
        .map(|index| index.get::<String, _>("name"))
        .filter(|name| name.starts_with("runs_by_"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        index_names.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "runs_by_automation",
            "runs_by_created_at",
            "runs_by_parent",
            "runs_by_status",
            "runs_by_updated_at",
        ]
    );

    Ok(())
}

/// A database written before the event log was dropped keeps its run rows:
/// the migration rebuilds the table and copies every surviving column.
#[tokio::test]
async fn dropping_the_event_log_keeps_the_run_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    // Rewind to the schema an older binary left: the `runs` table with its
    // legacy columns, the event log referencing it and the activation
    // bookkeeping, with only the drop migration pending again. Those
    // migrations' own rows stay applied, so sqlx's checksum validation
    // still passes.
    sqlx::raw_sql("DROP TABLE runs; DELETE FROM _sqlx_migrations WHERE version = 2026091803;")
        .execute(database.pool())
        .await?;
    for migration in [
        fabro_db::RUNS_MIGRATION_SQL,
        include_str!("../migrations/2026082701_run_events.sql"),
        include_str!("../migrations/2026082802_run_history_activation.sql"),
        include_str!("../migrations/2026083101_run_event_session_owner.sql"),
    ] {
        sqlx::raw_sql(migration).execute(database.pool()).await?;
    }
    sqlx::query(
        r#"
INSERT INTO runs (
    id, source_last_seq, created_at_ms, last_event_at_ms, status, title, input_tokens,
    diff_additions, total_usd_micros, summary_json
) VALUES ('kept', 7, 1, 2, 'succeeded', 'Kept run', 99, 3, 4, '{"id":"kept"}')
"#,
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO run_events (run_id, seq, event_name, event_json) VALUES ('kept', 1, 'run.created', '{}')",
    )
    .execute(database.pool())
    .await?;

    database.migrate().await?;

    assert!(!table_exists(database.pool(), "run_events").await?);
    let row = sqlx::query(
        "SELECT created_at_ms, last_event_at_ms, status, title, diff_additions, total_usd_micros, summary_json          FROM runs WHERE id = 'kept'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(row.get::<i64, _>("created_at_ms"), 1);
    assert_eq!(row.get::<i64, _>("last_event_at_ms"), 2);
    assert_eq!(row.get::<String, _>("status"), "succeeded");
    assert_eq!(row.get::<String, _>("title"), "Kept run");
    assert_eq!(row.get::<i64, _>("diff_additions"), 3);
    assert_eq!(row.get::<i64, _>("total_usd_micros"), 4);
    assert_eq!(row.get::<String, _>("summary_json"), r#"{"id":"kept"}"#);
    Ok(())
}

async fn insert_minimal_run(
    pool: &fabro_db::DbPool,
    status: &str,
    diff_additions: i64,
    summary_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
INSERT INTO runs (
    id, created_at_ms, last_event_at_ms, status, title, diff_additions, summary_json
) VALUES (?, 0, 0, ?, 'title', ?, ?)
",
    )
    .bind(format!("run-{status}-{diff_additions}"))
    .bind(status)
    .bind(diff_additions)
    .bind(summary_json)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn auth_sessions_schema_enforces_one_live_token_and_cascade() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    let session = "11111111-1111-4111-8111-111111111111";
    insert_auth_session(database.pool(), session, "https://github.com", "12345").await?;
    insert_refresh_token(database.pool(), &[1_u8; 32], session, 1_000, None).await?;

    // Rotation marks the old token used before issuing the new one, so a
    // second live token in the same chain must be impossible.
    assert!(
        insert_refresh_token(database.pool(), &[2_u8; 32], session, 1_000, None)
            .await
            .is_err(),
        "a session must not hold two live refresh tokens"
    );
    // A used token alongside the live one is the normal post-rotation state.
    insert_refresh_token(database.pool(), &[2_u8; 32], session, 1_000, Some(1_500)).await?;

    assert!(
        insert_refresh_token(
            database.pool(),
            &[3_u8; 32],
            "22222222-2222-4222-8222-222222222222",
            1_000,
            None
        )
        .await
        .is_err(),
        "a refresh token must reference an existing session"
    );

    sqlx::query("DELETE FROM auth_sessions WHERE id = ?")
        .bind(session)
        .execute(database.pool())
        .await?;
    let orphaned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM refresh_tokens")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        orphaned, 0,
        "deleting a session should cascade to its tokens"
    );

    Ok(())
}

#[tokio::test]
async fn auth_sessions_schema_rejects_invalid_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    for (id, issuer, subject) in [
        ("too-short", "https://github.com", "12345"),
        ("33333333-3333-4333-8333-333333333333", "", "12345"),
        (
            "44444444-4444-4444-8444-444444444444",
            "https://github.com",
            "",
        ),
    ] {
        assert!(
            insert_auth_session(database.pool(), id, issuer, subject)
                .await
                .is_err(),
            "auth session row should be rejected: id={id}, issuer={issuer}, subject={subject}"
        );
    }

    let session = "55555555-5555-4555-8555-555555555555";
    insert_auth_session(database.pool(), session, "https://github.com", "12345").await?;
    for (hash, expires_at_ms, used_at_ms) in
        [(vec![9_u8; 31], 1_000, None), (vec![9_u8; 32], 0, None)]
    {
        assert!(
            insert_refresh_token(database.pool(), &hash, session, expires_at_ms, used_at_ms)
                .await
                .is_err(),
            "refresh token row should be rejected: len={}, expires_at_ms={expires_at_ms}",
            hash.len()
        );
    }

    Ok(())
}

#[tokio::test]
async fn authorization_code_schema_enforces_hash_identity_and_expiry_index() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    let columns = sqlx::query("PRAGMA table_info(oauth_authorization_codes)")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(columns.len(), 10);
    assert_eq!(columns[0].get::<String, _>("name"), "code_hash");
    assert_eq!(columns[0].get::<String, _>("type"), "BLOB");
    assert_eq!(columns[0].get::<i64, _>("notnull"), 1);
    assert_eq!(columns[0].get::<i64, _>("pk"), 1);

    insert_authorization_code(database.pool(), &[1_u8; 32], "https://github.com", "12345").await?;
    for (hash, issuer, subject) in [
        (vec![2_u8; 31], "https://github.com", "12345"),
        (vec![2_u8; 33], "https://github.com", "12345"),
        (vec![2_u8; 32], "", "12345"),
        (vec![2_u8; 32], "https://github.com", ""),
    ] {
        assert!(
            insert_authorization_code(database.pool(), &hash, issuer, subject)
                .await
                .is_err(),
            "invalid authorization code row should be rejected: hash_len={}, issuer={issuer:?}, subject={subject:?}",
            hash.len()
        );
    }

    let expiry_index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'index' AND name = 'oauth_authorization_codes_by_expiry'",
    )
    .fetch_optional(database.pool())
    .await?;
    assert_eq!(
        expiry_index.as_deref(),
        Some("oauth_authorization_codes_by_expiry")
    );

    Ok(())
}

async fn insert_authorization_code(
    pool: &fabro_db::DbPool,
    code_hash: &[u8],
    identity_issuer: &str,
    identity_subject: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
INSERT INTO oauth_authorization_codes (
    code_hash, identity_issuer, identity_subject, login, name, email,
    code_challenge, redirect_uri, expires_at_ms
) VALUES (?, ?, ?, 'octocat', 'The Octocat', 'octocat@example.com',
          'challenge', 'http://127.0.0.1/callback', 1000)
",
    )
    .bind(code_hash)
    .bind(identity_issuer)
    .bind(identity_subject)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_auth_session(
    pool: &fabro_db::DbPool,
    id: &str,
    identity_issuer: &str,
    identity_subject: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
INSERT INTO auth_sessions (
    id, identity_issuer, identity_subject, login, name, email,
    created_at_ms, last_used_at_ms
) VALUES (?, ?, ?, 'octocat', 'The Octocat', 'octocat@example.com', 0, 0)
",
    )
    .bind(id)
    .bind(identity_issuer)
    .bind(identity_subject)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_refresh_token(
    pool: &fabro_db::DbPool,
    token_hash: &[u8],
    session_id: &str,
    expires_at_ms: i64,
    used_at_ms: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
INSERT INTO refresh_tokens (token_hash, session_id, issued_at_ms, expires_at_ms, used_at_ms)
VALUES (?, ?, 0, ?, ?)
",
    )
    .bind(token_hash)
    .bind(session_id)
    .bind(expires_at_ms)
    .bind(used_at_ms)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn environments_schema_rejects_invalid_rows() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;

    insert_minimal_environment(database.pool(), "valid", "docker", "allow_all").await?;
    // Any well-formed sandbox-driver kind name is a valid provider: plugins
    // are configured by kind, not enumerated in the schema.
    insert_minimal_environment(database.pool(), "plugin", "e2b-cloud", "allow_all").await?;

    for (id, provider, network_mode) in [
        ("Bad", "docker", "allow_all"),
        ("local", "docker", "allow_all"),
        ("bad-provider", "Bogus Provider", "allow_all"),
        ("bad-provider-hyphen", "-e2b", "allow_all"),
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

#[tokio::test]
async fn fresh_database_migrate_takes_no_snapshot() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fabro.sqlite3");

    let database = fabro_db::Database::connect(&db_path).await?;
    database.migrate().await?;

    assert!(
        !fabro_db::pre_migration_snapshot_path(&db_path).exists(),
        "a fresh database has no pre-migration state worth snapshotting"
    );
    Ok(())
}

// Simulates a binary upgrade: a database whose `_sqlx_migrations` table is
// missing an entry for a bundled migration is exactly what an older binary
// leaves behind for a newer one. The environments migration is pure CREATE
// TABLE, so dropping the table and deleting its version row makes it pending
// again without violating checksums.
#[tokio::test]
async fn migrate_snapshots_database_before_applying_new_migrations() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fabro.sqlite3");
    let snapshot_path = fabro_db::pre_migration_snapshot_path(&db_path);

    let database = fabro_db::Database::connect(&db_path).await?;
    database.migrate().await?;
    sqlx::query(
        "INSERT INTO variables (name, value, created_at, updated_at) \
         VALUES ('SNAPSHOT_MARKER', 'kept', '2026-07-22T00:00:00Z', '2026-07-22T00:00:00Z')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DROP TABLE environments")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 2026063002")
        .execute(database.pool())
        .await?;

    database.migrate().await?;

    assert!(
        snapshot_path.exists(),
        "pending migration must snapshot first"
    );
    let snapshot = connect_read_only(&snapshot_path).await?;
    assert!(
        !table_exists(&snapshot, "environments").await?,
        "snapshot must hold the pre-migration schema"
    );
    let snapshot_marker: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM variables WHERE name = 'SNAPSHOT_MARKER'")
            .fetch_one(&snapshot)
            .await?;
    assert_eq!(snapshot_marker, 1, "snapshot must preserve row data");
    snapshot.close().await;

    assert!(
        table_exists(database.pool(), "environments").await?,
        "migration must still apply"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&snapshot_path)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "snapshot must be private");
    }

    // With nothing pending, migrate must not rewrite the snapshot: it still
    // holds the state from before the most recent schema change.
    database.migrate().await?;
    let snapshot = connect_read_only(&snapshot_path).await?;
    assert!(
        !table_exists(&snapshot, "environments").await?,
        "no-pending migrate must leave the snapshot untouched"
    );
    snapshot.close().await;
    Ok(())
}

async fn connect_read_only(path: &std::path::Path) -> anyhow::Result<sqlx::SqlitePool> {
    Ok(sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=ro", path.display())).await?)
}

async fn table_exists(pool: &sqlx::SqlitePool, table: &str) -> anyhow::Result<bool> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_one(pool)
            .await?;
    Ok(count == 1)
}
