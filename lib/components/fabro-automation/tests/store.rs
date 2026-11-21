#![expect(
    clippy::unwrap_used,
    reason = "SQLite automation-store integration tests use panic-on-failure fixture setup"
)]

use std::path::Path;

use fabro_automation::{
    ApiTrigger, AutomationDraft, AutomationGitWorkflowSource, AutomationId, AutomationReplace,
    AutomationRevision, AutomationStore, AutomationStoreError, AutomationTrigger,
    AutomationTriggerId, JiraTrigger, ScheduleTrigger,
};
use fabro_db::Database;
use fabro_types::{GitRunTarget, RunTarget};
use sqlx::Row as _;
use tokio::fs;

async fn test_database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let database = Database::connect(dir.path().join("fabro.sqlite3"))
        .await
        .unwrap();
    database.migrate().await.unwrap();
    insert_environment(database.pool(), "default", "docker").await;
    (dir, database)
}

fn target() -> RunTarget {
    RunTarget::Git(GitRunTarget {
        repo:   "fabro-sh/fabro".to_string(),
        branch: "main".to_string(),
        tag:    None,
        sha:    None,
    })
}

fn schedule(id: &str, expression: &str, enabled: bool) -> AutomationTrigger {
    AutomationTrigger::Schedule(ScheduleTrigger {
        id: AutomationTriggerId::new(id).unwrap(),
        enabled,
        expression: expression.to_string(),
    })
}

fn jira(id: &str, jql: &str, in_progress: Option<&str>) -> AutomationTrigger {
    AutomationTrigger::Jira(JiraTrigger {
        id:                 AutomationTriggerId::new(id).unwrap(),
        enabled:            true,
        jql:                jql.to_string(),
        in_progress_status: in_progress.map(str::to_string),
        review_status:      None,
    })
}

fn workflow_source(
    branch: &str,
    tag: Option<&str>,
    sha: Option<&str>,
) -> AutomationGitWorkflowSource {
    AutomationGitWorkflowSource {
        repo:   "fabro-sh/workflows".to_string(),
        branch: branch.to_string(),
        tag:    tag.map(str::to_string),
        sha:    sha.map(str::to_string),
    }
}

fn draft(id: &str, api_enabled: bool) -> AutomationDraft {
    AutomationDraft {
        id:              AutomationId::new(id).unwrap(),
        name:            "Nightly".to_string(),
        description:     Some("Runs every night".to_string()),
        environment_id:  Some("default".to_string()),
        target:          target(),
        workflow:        "release".to_string(),
        workflow_source: None,
        triggers:        vec![
            schedule("z-last", "0 2 * * *", false),
            AutomationTrigger::Api(ApiTrigger {
                id:      AutomationTriggerId::new("custom-api-id").unwrap(),
                enabled: api_enabled,
            }),
            schedule("a-first", "0 1 * * *", true),
        ],
    }
}

fn replacement(name: &str, expression: &str) -> AutomationReplace {
    AutomationReplace {
        name:            name.to_string(),
        description:     None,
        environment_id:  Some("default".to_string()),
        target:          target(),
        workflow:        "release".to_string(),
        workflow_source: None,
        triggers:        vec![
            schedule("nightly", expression, true),
            AutomationTrigger::Api(ApiTrigger {
                id:      AutomationTriggerId::new("api").unwrap(),
                enabled: true,
            }),
        ],
    }
}

fn trigger_ids(automation: &fabro_automation::Automation) -> Vec<&str> {
    automation
        .triggers
        .iter()
        .map(|trigger| trigger.id().as_str())
        .collect()
}

#[tokio::test]
async fn crud_normalizes_api_and_schedule_order() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());

    let created = store.create(draft("nightly", true)).await.unwrap();
    assert_eq!(created.environment_id.as_deref(), Some("default"));
    assert_eq!(trigger_ids(&created), vec!["manual", "a-first", "z-last"]);
    assert!(created.enabled_api_trigger().is_some());

    let fetched = store.get(&created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(store.list().await.unwrap(), vec![created.clone()]);

    let replaced = store
        .replace(
            &created.id,
            &created.revision,
            replacement("Updated", "30 4 * * *"),
        )
        .await
        .unwrap();
    assert_ne!(replaced.revision, created.revision);
    assert_eq!(replaced.name, "Updated");

    store
        .delete(&replaced.id, &replaced.revision)
        .await
        .unwrap();
    assert!(store.get(&replaced.id).await.unwrap().is_none());
}

#[tokio::test]
async fn create_requires_an_environment_and_environment_changes_revision() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let mut missing = draft("missing", true);
    missing.environment_id = None;

    let error = store.create(missing).await.unwrap_err();
    assert!(matches!(error, AutomationStoreError::Validation { .. }));

    let created = store.create(draft("nightly", true)).await.unwrap();
    insert_environment(database.pool(), "daytona-smoke", "daytona").await;
    let mut update = replacement("Nightly", "0 1 * * *");
    update.environment_id = Some("daytona-smoke".to_string());
    let replaced = store
        .replace(&created.id, &created.revision, update)
        .await
        .unwrap();

    assert_eq!(replaced.environment_id.as_deref(), Some("daytona-smoke"));
    assert_ne!(replaced.revision, created.revision);

    store
        .set_last_error(&replaced.id, Some("environment unavailable"))
        .await
        .unwrap();
    let failed = store.get(&replaced.id).await.unwrap().unwrap();
    assert_eq!(
        failed.last_error.as_deref(),
        Some("environment unavailable")
    );
    assert_eq!(failed.revision, replaced.revision);

    store.set_last_error(&replaced.id, None).await.unwrap();
    assert_eq!(
        store.get(&replaced.id).await.unwrap().unwrap().last_error,
        None,
    );
}

#[tokio::test]
async fn legacy_environment_backfill_prefers_default_then_a_single_compatible_environment() {
    let (_dir, database) = test_database().await;
    insert_incomplete_automation(database.pool(), "with-default").await;
    insert_environment(database.pool(), "daytona-smoke", "daytona").await;

    let report = fabro_automation::backfill_environment_selectors(database.pool())
        .await
        .unwrap();
    let store = AutomationStore::new(database.clone_pool());
    let migrated = store
        .get(&AutomationId::new("with-default").unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(report.updated_rows, 1);
    assert_eq!(report.environment_id.as_deref(), Some("default"));
    assert_eq!(migrated.environment_id.as_deref(), Some("default"));
    assert_ne!(migrated.revision.as_str(), &"a".repeat(64));

    sqlx::query("DELETE FROM automations WHERE id = 'with-default'")
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM environments WHERE id = 'default'")
        .execute(database.pool())
        .await
        .unwrap();
    insert_incomplete_automation(database.pool(), "single").await;
    let report = fabro_automation::backfill_environment_selectors(database.pool())
        .await
        .unwrap();
    let migrated = store
        .get(&AutomationId::new("single").unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(report.environment_id.as_deref(), Some("daytona-smoke"));
    assert_eq!(migrated.environment_id.as_deref(), Some("daytona-smoke"));
}

#[tokio::test]
async fn legacy_environment_backfill_leaves_ambiguous_or_empty_catalogs_incomplete() {
    let (_dir, database) = test_database().await;
    sqlx::query("DELETE FROM environments WHERE id = 'default'")
        .execute(database.pool())
        .await
        .unwrap();
    insert_incomplete_automation(database.pool(), "empty").await;

    let empty = fabro_automation::backfill_environment_selectors(database.pool())
        .await
        .unwrap();
    assert_eq!(empty.updated_rows, 0);
    assert_eq!(empty.environment_id, None);

    insert_environment(database.pool(), "docker-one", "docker").await;
    insert_environment(database.pool(), "daytona-two", "daytona").await;
    insert_incomplete_automation(database.pool(), "ambiguous").await;
    let ambiguous = fabro_automation::backfill_environment_selectors(database.pool())
        .await
        .unwrap();
    let store = AutomationStore::new(database.clone_pool());

    assert_eq!(ambiguous.updated_rows, 0);
    assert_eq!(ambiguous.environment_id, None);
    assert_eq!(
        store
            .get(&AutomationId::new("ambiguous").unwrap())
            .await
            .unwrap()
            .unwrap()
            .environment_id,
        None,
    );
}

async fn insert_incomplete_automation(pool: &fabro_db::DbPool, id: &str) {
    sqlx::query(
        r"
        INSERT INTO automations (
            id, revision, name, api_enabled, target_repository, target_branch,
            target_tag, target_sha, target_workflow, environment_id
        ) VALUES (?, ?, 'Legacy', 1, 'fabro-sh/fabro', 'main', NULL, NULL, 'release', NULL)
        ",
    )
    .bind(id)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_environment(pool: &fabro_db::DbPool, id: &str, provider: &str) {
    sqlx::query(
        r"
        INSERT INTO environments (
            id, revision, provider, network_mode,
            lifecycle_preserve, lifecycle_stop_on_terminal
        ) VALUES (?, ?, ?, 'allow_all', 0, 1)
        ",
    )
    .bind(id)
    .bind("b".repeat(64))
    .bind(provider)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn crud_round_trips_workflow_source_selectors_and_clears_to_omission() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());

    for (index, source) in [
        workflow_source("main", None, None),
        workflow_source("main", Some("release/v1"), None),
        workflow_source(
            "context-only",
            Some("release/v1"),
            Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("source-{index}");
        let mut value = draft(&id, true);
        value.workflow_source = Some(source);
        let created = store.create(value).await.unwrap();
        assert_eq!(
            created
                .workflow_source
                .as_ref()
                .and_then(|source| source.sha.as_deref()),
            (index == 2).then_some("abcdef0123456789abcdef0123456789abcdef01")
        );
        assert_eq!(store.get(&created.id).await.unwrap(), Some(created.clone()));

        let mut cleared = replacement("Cleared", "30 4 * * *");
        cleared.workflow_source = None;
        let replaced = store
            .replace(&created.id, &created.revision, cleared)
            .await
            .unwrap();
        assert_eq!(replaced.workflow_source, None);
        let columns = sqlx::query(
            "SELECT workflow_source_repository, workflow_source_branch, workflow_source_tag, \
             workflow_source_sha \
             FROM automations WHERE id = ?",
        )
        .bind(created.id.as_str())
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(
            columns.get::<Option<String>, _>("workflow_source_repository"),
            None
        );
        assert_eq!(
            columns.get::<Option<String>, _>("workflow_source_branch"),
            None
        );
        assert_eq!(
            columns.get::<Option<String>, _>("workflow_source_tag"),
            None
        );
        assert_eq!(
            columns.get::<Option<String>, _>("workflow_source_sha"),
            None
        );
    }

    assert_eq!(store.list().await.unwrap().len(), 3);
}

#[tokio::test]
async fn corrupt_workflow_source_rows_are_rejected_as_stored_shape_errors() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let partial = store.create(draft("partial", true)).await.unwrap();
    let orphan = store.create(draft("orphan", true)).await.unwrap();

    let mut connection = database.pool().acquire().await.unwrap();
    sqlx::query("DROP TRIGGER automation_workflow_source_all_or_none_update")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE automations SET workflow_source_repository = 'fabro-sh/workflows' WHERE id = ?",
    )
    .bind(partial.id.as_str())
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query("UPDATE automations SET workflow_source_tag = 'v1' WHERE id = ?")
        .bind(orphan.id.as_str())
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    assert!(matches!(
        store.get(&partial.id).await.unwrap_err(),
        AutomationStoreError::StoredWorkflowSourceShape { .. }
    ));
    assert!(matches!(
        store.get(&orphan.id).await.unwrap_err(),
        AutomationStoreError::StoredWorkflowSourceShape { .. }
    ));
}

#[tokio::test]
async fn disabled_api_trigger_normalizes_to_absent() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());

    let created = store.create(draft("nightly", false)).await.unwrap();

    assert!(created.enabled_api_trigger().is_none());
    assert_eq!(trigger_ids(&created), vec!["a-first", "z-last"]);
    assert_eq!(store.get(&created.id).await.unwrap().unwrap(), created);
}

#[tokio::test]
async fn equivalent_trigger_orders_have_the_same_revision() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let first = store.create(draft("first", true)).await.unwrap();
    let mut reordered = draft("second", true);
    reordered.triggers.reverse();

    let second = store.create(reordered).await.unwrap();

    assert_eq!(first.revision, second.revision);
}

#[tokio::test]
async fn create_conflict_and_conditional_delete_errors_are_typed() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let created = store.create(draft("nightly", true)).await.unwrap();

    let duplicate = store.create(draft("nightly", true)).await.unwrap_err();
    assert!(matches!(
        duplicate,
        AutomationStoreError::AlreadyExists { .. }
    ));

    let mut revision_source = draft("revision-source", true);
    revision_source.name = "Different revision".to_string();
    let stale_revision = store.create(revision_source).await.unwrap().revision;
    let stale = store
        .delete(&created.id, &stale_revision)
        .await
        .unwrap_err();
    assert!(matches!(stale, AutomationStoreError::StaleRevision { .. }));

    let missing = AutomationId::new("missing").unwrap();
    let not_found = store.delete(&missing, &stale_revision).await.unwrap_err();
    assert!(matches!(not_found, AutomationStoreError::NotFound { .. }));
}

#[tokio::test]
async fn independent_pools_observe_writes_and_revision_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fabro.sqlite3");
    let first_database = Database::connect(&path).await.unwrap();
    first_database.migrate().await.unwrap();
    insert_environment(first_database.pool(), "default", "docker").await;
    let second_database = Database::connect(&path).await.unwrap();
    second_database.migrate().await.unwrap();
    let first = AutomationStore::new(first_database.clone_pool());
    let second = AutomationStore::new(second_database.clone_pool());

    let created = first.create(draft("nightly", true)).await.unwrap();
    assert_eq!(second.get(&created.id).await.unwrap().unwrap(), created);

    let replaced = second
        .replace(
            &created.id,
            &created.revision,
            replacement("Winner", "0 5 * * *"),
        )
        .await
        .unwrap();
    let err = first
        .replace(
            &created.id,
            &created.revision,
            replacement("Loser", "0 6 * * *"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AutomationStoreError::StaleRevision {
        actual,
        ..
    } if actual == replaced.revision));
}

#[tokio::test]
async fn failed_schedule_insert_rolls_back_parent_replace() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let created = store.create(draft("nightly", true)).await.unwrap();
    sqlx::query(
        r"
        CREATE TRIGGER reject_blocked_schedule
        BEFORE INSERT ON automation_triggers
        WHEN NEW.id = 'blocked'
        BEGIN
            SELECT RAISE(ABORT, 'blocked schedule');
        END
        ",
    )
    .execute(database.pool())
    .await
    .unwrap();
    let replacement = AutomationReplace {
        name:            "Should roll back".to_string(),
        description:     None,
        environment_id:  Some("default".to_string()),
        target:          target(),
        workflow:        "release".to_string(),
        workflow_source: None,
        triggers:        vec![schedule("blocked", "0 7 * * *", true)],
    };

    let err = store
        .replace(&created.id, &created.revision, replacement)
        .await
        .unwrap_err();

    assert!(matches!(err, AutomationStoreError::Db { .. }));
    assert_eq!(store.get(&created.id).await.unwrap().unwrap(), created);
}

#[tokio::test]
async fn invalid_stored_schedule_is_rejected_on_read() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let created = store.create(draft("nightly", true)).await.unwrap();
    sqlx::query("UPDATE automation_triggers SET expression = 'not cron' WHERE automation_id = ?")
        .bind(created.id.as_str())
        .execute(database.pool())
        .await
        .unwrap();

    let err = store.get(&created.id).await.unwrap_err();

    assert!(matches!(err, AutomationStoreError::StoredValidation { .. }));
}

#[tokio::test]
async fn legacy_import_is_transactional_and_sql_wins() {
    let (dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    store.create(draft("existing", true)).await.unwrap();
    let source_dir = dir.path().join("automations");
    fs::create_dir_all(&source_dir).await.unwrap();
    write_legacy_automation(&source_dir, "existing", "Legacy existing").await;
    let imported_bytes = write_legacy_automation(&source_dir, "imported", "Imported").await;
    let expected_revision = AutomationRevision::from_bytes(&imported_bytes);
    fs::write(source_dir.join("notes.txt"), "ignored")
        .await
        .unwrap();

    let report = fabro_automation::import_legacy_directory_once(database.pool(), &source_dir)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(report.imported_rows, 1);
    assert_eq!(report.skipped_rows, 1);
    assert_eq!(report.names, vec!["imported"]);
    assert!(!source_dir.exists());
    assert!(report.backup_path.exists());
    assert_eq!(
        store
            .get(&AutomationId::new("existing").unwrap())
            .await
            .unwrap()
            .unwrap()
            .name,
        "Nightly"
    );
    let imported = store
        .get(&AutomationId::new("imported").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(imported.name, "Imported");
    assert_eq!(imported.revision, expected_revision);
    assert_eq!(imported.workflow, "release");
    assert_eq!(imported.workflow_source, None);
    assert!(matches!(
        imported.target,
        RunTarget::Git(GitRunTarget {
            branch,
            tag: None,
            sha: None,
            ..
        }) if branch == "main"
    ));

    fs::create_dir_all(&source_dir).await.unwrap();
    write_legacy_automation(&source_dir, "existing", "Legacy existing").await;
    write_legacy_automation(&source_dir, "imported", "Imported again").await;
    let retry = fabro_automation::import_legacy_directory_once(database.pool(), &source_dir)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.imported_rows, 0);
    assert_eq!(retry.skipped_rows, 2);
    assert!(retry.backup_path.exists());
    assert_eq!(
        store
            .get(&AutomationId::new("imported").unwrap())
            .await
            .unwrap()
            .unwrap()
            .name,
        "Imported"
    );
    assert!(
        fabro_automation::import_legacy_directory_once(database.pool(), &source_dir)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn invalid_legacy_file_leaves_directory_and_database_unchanged() {
    let (dir, database) = test_database().await;
    let source_dir = dir.path().join("automations");
    fs::create_dir_all(&source_dir).await.unwrap();
    write_legacy_automation(&source_dir, "valid", "Valid").await;
    fs::write(source_dir.join("broken.toml"), "not valid toml =")
        .await
        .unwrap();

    let err = fabro_automation::import_legacy_directory_once(database.pool(), &source_dir)
        .await
        .unwrap_err();

    assert!(matches!(err, AutomationStoreError::Parse { .. }));
    assert!(source_dir.exists());
    assert!(
        AutomationStore::new(database.clone_pool())
            .list()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unsupported_legacy_target_leaves_directory_and_database_unchanged() {
    let (dir, database) = test_database().await;
    let source_dir = dir.path().join("automations");
    fs::create_dir_all(&source_dir).await.unwrap();
    let bytes = legacy_automation_bytes("Unsupported", "refs/pull/123/head");
    fs::write(source_dir.join("unsupported.toml"), bytes)
        .await
        .unwrap();

    let err = fabro_automation::import_legacy_directory_once(database.pool(), &source_dir)
        .await
        .unwrap_err();

    assert!(matches!(err, AutomationStoreError::LegacyTarget { .. }));
    assert!(err.to_string().contains("edit target.ref"));
    assert!(source_dir.exists());
    assert!(
        AutomationStore::new(database.clone_pool())
            .list()
            .await
            .unwrap()
            .is_empty()
    );
}

async fn write_legacy_automation(dir: &Path, id: &str, name: &str) -> Vec<u8> {
    let bytes = legacy_automation_bytes(name, "main");
    fs::write(dir.join(format!("{id}.toml")), &bytes)
        .await
        .unwrap();
    bytes
}

fn legacy_automation_bytes(name: &str, ref_selector: &str) -> Vec<u8> {
    format!(
        r#"name = "{name}"

[target]
repository = "fabro-sh/fabro"
ref = "{ref_selector}"
workflow = "release"

[[triggers]]
id = "manual"
type = "api"
enabled = true

[[triggers]]
id = "nightly"
type = "schedule"
enabled = true
expression = "0 3 * * *"
"#
    )
    .into_bytes()
}

#[tokio::test]
async fn jira_trigger_persists_and_loads() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let mut draft = draft("board", true);
    draft
        .triggers
        .push(jira("jira", "project = ABC", Some("In Progress")));

    let created = store.create(draft).await.unwrap();
    assert_eq!(trigger_ids(&created), vec![
        "manual", "a-first", "z-last", "jira"
    ]);
    let fetched = store.get(&created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(store.list().await.unwrap(), vec![created.clone()]);
    let trigger = fetched.enabled_jira_trigger().unwrap();
    assert_eq!(trigger.jql, "project = ABC");
    assert_eq!(trigger.in_progress_status.as_deref(), Some("In Progress"));
    assert_eq!(trigger.review_status, None);
}

#[tokio::test]
async fn replace_swaps_jira_trigger() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let mut first = draft("board", false);
    first.triggers.push(jira("jira", "project = ABC", None));
    let created = store.create(first).await.unwrap();

    let mut next = replacement("Updated", "30 4 * * *");
    next.triggers
        .push(jira("other", "project = XYZ", Some("Doing")));
    let replaced = store
        .replace(&created.id, &created.revision, next)
        .await
        .unwrap();
    assert_eq!(store.get(&created.id).await.unwrap().unwrap(), replaced);
    assert_eq!(trigger_ids(&replaced), vec!["manual", "nightly", "other"]);

    let mut without = replacement("Updated", "30 4 * * *");
    without.triggers.truncate(1);
    let cleared = store
        .replace(&replaced.id, &replaced.revision, without)
        .await
        .unwrap();
    let fetched = store.get(&cleared.id).await.unwrap().unwrap();
    assert!(fetched.enabled_jira_trigger().is_none());
}
