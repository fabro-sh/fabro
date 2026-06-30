use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fabro_environment::{
    EnvironmentDraft, EnvironmentId, EnvironmentStore, EnvironmentStoreError,
    import_legacy_directory_once, seed_default_environment, seed_environments,
};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    DockerfileSource, EnvironmentImageSettings, EnvironmentLifecycleSettings,
    EnvironmentNetworkMode, EnvironmentNetworkSettings, EnvironmentProvider,
    EnvironmentResourcesSettings, EnvironmentSettings,
};
use tokio::fs;

struct TestStore {
    dir:   tempfile::TempDir,
    pool:  fabro_db::DbPool,
    store: EnvironmentStore,
}

async fn test_store(local_enabled: bool) -> anyhow::Result<TestStore> {
    let dir = tempfile::tempdir()?;
    let database = fabro_db::Database::connect(dir.path().join("fabro.sqlite3")).await?;
    database.migrate().await?;
    let pool = database.clone_pool();
    let store = EnvironmentStore::load(pool.clone(), local_enabled).await?;
    Ok(TestStore { dir, pool, store })
}

fn settings(provider: EnvironmentProvider) -> EnvironmentSettings {
    EnvironmentSettings {
        provider,
        cwd: None,
        image: EnvironmentImageSettings::default(),
        resources: EnvironmentResourcesSettings::default(),
        network: EnvironmentNetworkSettings::default(),
        lifecycle: EnvironmentLifecycleSettings::default(),
        labels: HashMap::new(),
        env: HashMap::new(),
    }
}

fn draft(id: &str, provider: EnvironmentProvider) -> EnvironmentDraft {
    EnvironmentDraft {
        id:       EnvironmentId::new(id).expect("test environment id should be valid"),
        settings: settings(provider),
    }
}

#[tokio::test]
async fn new_store_lists_only_synthetic_local_when_enabled() -> anyhow::Result<()> {
    let enabled = test_store(true).await?;
    assert_eq!(
        environment_ids(&enabled.store),
        vec!["local"],
        "local should be synthesized, not persisted"
    );
    assert_eq!(sql_environment_count(&enabled.pool).await?, 0);

    let disabled = test_store(false).await?;
    assert!(disabled.store.list().is_empty());

    Ok(())
}

#[tokio::test]
async fn seed_default_is_idempotent_and_reopen_loads_sql_rows() -> anyhow::Result<()> {
    let test = test_store(true).await?;

    seed_environments(&test.pool).await?;
    seed_environments(&test.pool).await?;

    let reopened = EnvironmentStore::load(test.pool.clone(), true).await?;
    assert_eq!(environment_ids(&reopened), vec!["default", "local"]);
    assert_eq!(sql_environment_count(&test.pool).await?, 1);

    Ok(())
}

#[tokio::test]
async fn create_get_replace_delete_and_reload_round_trip_sql_rows() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    let created = test
        .store
        .create(draft("custom", EnvironmentProvider::Docker))
        .await?;

    assert_eq!(created.id.as_str(), "custom");
    assert_eq!(
        test.store
            .get(&EnvironmentId::new("custom").expect("valid id"))
            .expect("created environment should be cached")
            .revision,
        created.revision
    );

    let reopened = EnvironmentStore::load(test.pool.clone(), true).await?;
    assert_eq!(
        reopened
            .get(&EnvironmentId::new("custom").expect("valid id"))
            .expect("created environment should reload")
            .revision,
        created.revision
    );

    let mut replacement = settings(EnvironmentProvider::Local);
    replacement.cwd = Some("/workspace/custom".to_string());
    replacement
        .labels
        .insert("tier".to_string(), "dev".to_string());
    let replaced = test
        .store
        .replace(&created.id, &created.revision, replacement)
        .await?;
    assert_ne!(replaced.revision, created.revision);
    assert_eq!(replaced.settings.cwd.as_deref(), Some("/workspace/custom"));

    let stale = test
        .store
        .replace(
            &created.id,
            &created.revision,
            settings(EnvironmentProvider::Docker),
        )
        .await
        .expect_err("stale revision should be rejected");
    assert!(matches!(stale, EnvironmentStoreError::StaleRevision { .. }));

    test.store.delete(&created.id, &replaced.revision).await?;
    assert!(test.store.get(&created.id).is_none());
    assert!(
        EnvironmentStore::load(test.pool.clone(), true)
            .await?
            .get(&created.id)
            .is_none()
    );

    Ok(())
}

#[tokio::test]
async fn default_is_deletable() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    seed_default_environment(&test.pool, EnvironmentProvider::Docker).await?;
    let store = EnvironmentStore::load(test.pool.clone(), true).await?;
    let default = store
        .get(&EnvironmentId::new("default").expect("valid id"))
        .expect("default should be seeded");

    store.delete(&default.id, &default.revision).await?;

    assert!(store.get(&default.id).is_none());
    assert_eq!(sql_environment_count(&test.pool).await?, 0);

    Ok(())
}

#[tokio::test]
async fn maps_network_lifecycle_and_inline_dockerfile_round_trip() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    let mut settings = settings(EnvironmentProvider::Daytona);
    settings.image.dockerfile = Some(DockerfileSource::Inline("FROM alpine\n".to_string()));
    settings.resources.cpu = Some(4);
    settings.resources.memory = Some("8GB".parse()?);
    settings.resources.disk = Some("20GB".parse()?);
    settings.network.mode = EnvironmentNetworkMode::CidrAllowList;
    settings.network.allow = vec!["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()];
    settings.lifecycle.preserve = true;
    settings.lifecycle.stop_on_terminal = false;
    settings.lifecycle.auto_stop = Some("30m".parse()?);
    settings
        .labels
        .insert("team".to_string(), "platform".to_string());
    settings.env.insert(
        "TOKEN".to_string(),
        InterpString::parse("Bearer {{ secrets.API_TOKEN }}"),
    );

    let created = test
        .store
        .create(EnvironmentDraft {
            id: EnvironmentId::new("rich").expect("valid id"),
            settings,
        })
        .await?;
    let reloaded = EnvironmentStore::load(test.pool.clone(), true)
        .await?
        .get(&created.id)
        .expect("rich environment should reload");

    assert_eq!(reloaded.settings, created.settings);

    Ok(())
}

#[tokio::test]
async fn direct_create_rejects_dockerfile_path_without_reading_it() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    let mut settings = settings(EnvironmentProvider::Docker);
    settings.image.dockerfile = Some(DockerfileSource::Path {
        path: test.dir.path().join("Dockerfile").display().to_string(),
    });

    let err = test
        .store
        .create(EnvironmentDraft {
            id: EnvironmentId::new("path").expect("valid id"),
            settings,
        })
        .await
        .expect_err("path Dockerfile should be rejected");

    assert!(matches!(err, EnvironmentStoreError::Validation { .. }));
    assert_eq!(sql_environment_count(&test.pool).await?, 0);

    Ok(())
}

#[tokio::test]
async fn legacy_import_missing_directory_is_noop() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    let report =
        import_legacy_directory_once(&test.pool, test.dir.path().join("environments")).await?;

    assert!(report.is_none());
    assert_eq!(sql_environment_count(&test.pool).await?, 0);

    Ok(())
}

#[tokio::test]
async fn legacy_import_imports_rows_renames_source_and_is_idempotent() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    let environment_dir = test.dir.path().join("environments");
    fs::create_dir(&environment_dir).await?;
    fs::write(
        environment_dir.join("cloud.toml"),
        r#"
provider = "docker"

[resources]
cpu = 3
"#,
    )
    .await?;
    fs::write(
        environment_dir.join("local.toml"),
        r#"
provider = "local"

[resources]
cpu = 99
"#,
    )
    .await?;

    let report = import_legacy_directory_once(&test.pool, &environment_dir)
        .await?
        .expect("legacy directory should import");
    let second = import_legacy_directory_once(&test.pool, &environment_dir).await?;

    assert_eq!(report.imported_rows, 1);
    assert_eq!(report.skipped_rows, 1);
    assert_eq!(report.environment_ids, vec!["cloud"]);
    assert!(second.is_none());
    assert!(!environment_dir.exists());
    assert!(report.backup_path.exists());

    let store = EnvironmentStore::load(test.pool.clone(), true).await?;
    assert_eq!(environment_ids(&store), vec!["cloud", "local"]);
    assert_eq!(
        store
            .get(&EnvironmentId::new("cloud").expect("valid id"))
            .expect("cloud should import")
            .settings
            .resources
            .cpu,
        Some(3)
    );
    assert_eq!(sql_environment_count(&test.pool).await?, 1);

    Ok(())
}

#[tokio::test]
async fn legacy_import_keeps_existing_sql_row_and_inlines_dockerfile_path() -> anyhow::Result<()> {
    let test = test_store(true).await?;
    test.store
        .create(draft("existing", EnvironmentProvider::Local))
        .await?;
    let environment_dir = test.dir.path().join("environments");
    fs::create_dir(&environment_dir).await?;
    fs::write(environment_dir.join("Dockerfile"), "FROM alpine\n").await?;
    fs::write(
        environment_dir.join("existing.toml"),
        r#"
provider = "docker"

[image.dockerfile]
path = "missing.Dockerfile"
"#,
    )
    .await?;
    fs::write(
        environment_dir.join("with-dockerfile.toml"),
        r#"
provider = "docker"

[image.dockerfile]
path = "Dockerfile"
"#,
    )
    .await?;

    let report = import_legacy_directory_once(&test.pool, &environment_dir)
        .await?
        .expect("legacy directory should import");

    assert_eq!(report.imported_rows, 1);
    assert_eq!(report.skipped_rows, 1);
    assert_eq!(report.environment_ids, vec!["with-dockerfile"]);

    let store = EnvironmentStore::load(test.pool.clone(), true).await?;
    assert_eq!(
        store
            .get(&EnvironmentId::new("existing").expect("valid id"))
            .expect("existing row should win")
            .settings
            .provider,
        EnvironmentProvider::Local
    );
    assert_eq!(
        store
            .get(&EnvironmentId::new("with-dockerfile").expect("valid id"))
            .expect("dockerfile row should import")
            .settings
            .image
            .dockerfile,
        Some(DockerfileSource::Inline("FROM alpine\n".to_string()))
    );

    Ok(())
}

#[tokio::test]
async fn legacy_import_invalid_input_leaves_source_directory_in_place() -> anyhow::Result<()> {
    assert_invalid_legacy_import_leaves_source_directory(
        "invalid filename",
        "Bad.toml",
        r#"provider = "local""#,
        "invalid_filename",
    )
    .await?;
    assert_invalid_legacy_import_leaves_source_directory(
        "invalid toml",
        "broken.toml",
        "provider = [",
        "parse",
    )
    .await?;
    assert_invalid_legacy_import_leaves_source_directory(
        "invalid settings",
        "invalid-settings.toml",
        r#"provider = "bogus""#,
        "validation",
    )
    .await?;

    Ok(())
}

async fn assert_invalid_legacy_import_leaves_source_directory(
    case: &str,
    file_name: &str,
    content: &str,
    expected_kind: &str,
) -> anyhow::Result<()> {
    let test = test_store(true).await?;
    let environment_dir = test.dir.path().join("environments");
    fs::create_dir(&environment_dir).await?;
    fs::write(environment_dir.join(file_name), content).await?;

    let Err(err) = import_legacy_directory_once(&test.pool, &environment_dir).await else {
        panic!("{case} should fail import");
    };

    assert_eq!(err.kind(), expected_kind, "{case} error kind");
    assert!(environment_dir.exists(), "{case} source dir should remain");
    assert!(
        legacy_backups(test.dir.path()).await?.is_empty(),
        "{case} should not create a backup"
    );
    assert_eq!(
        sql_environment_count(&test.pool).await?,
        0,
        "{case} should not import rows"
    );

    Ok(())
}

fn environment_ids(store: &EnvironmentStore) -> Vec<String> {
    store
        .list()
        .into_iter()
        .map(|environment| environment.id.to_string())
        .collect()
}

async fn sql_environment_count(pool: &fabro_db::DbPool) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM environments")
        .fetch_one(pool)
        .await?)
}

async fn legacy_backups(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(dir).await?;
    let mut backups = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.starts_with("environments.imported-")
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bak"))
        {
            backups.push(path);
        }
    }
    backups.sort();
    Ok(backups)
}
