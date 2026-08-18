#![expect(
    clippy::unwrap_used,
    reason = "SQLite automation-store integration tests use panic-on-failure fixture setup"
)]

use std::path::Path;

use fabro_automation::{
    ApiTrigger, AutomationDraft, AutomationId, AutomationReplace, AutomationStore,
    AutomationStoreError, AutomationTarget, AutomationTrigger, AutomationTriggerId, PlaneTrigger,
    ScheduleTrigger,
};
use fabro_db::Database;
use fabro_types::ExternalAgentHarness;
use tokio::fs;

async fn test_database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let database = Database::connect(dir.path().join("fabro.sqlite3"))
        .await
        .unwrap();
    database.migrate().await.unwrap();
    (dir, database)
}

fn target() -> AutomationTarget {
    AutomationTarget {
        repository:   "fabro-sh/fabro".to_string(),
        ref_selector: "main".to_string(),
        workflow:     "release".to_string(),
    }
}

fn schedule(id: &str, expression: &str, enabled: bool) -> AutomationTrigger {
    AutomationTrigger::Schedule(ScheduleTrigger {
        id: AutomationTriggerId::new(id).unwrap(),
        enabled,
        expression: expression.to_string(),
    })
}

fn draft(id: &str, api_enabled: bool) -> AutomationDraft {
    AutomationDraft {
        id:          AutomationId::new(id).unwrap(),
        name:        "Nightly".to_string(),
        description: Some("Runs every night".to_string()),
        target:      target(),
        triggers:    vec![
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
        name:        name.to_string(),
        description: None,
        target:      target(),
        triggers:    vec![
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
async fn plane_trigger_round_trips_through_sqlite() {
    let (_dir, database) = test_database().await;
    let store = AutomationStore::new(database.clone_pool());
    let created = store
        .create(AutomationDraft {
            id:          AutomationId::new("plane-loop").unwrap(),
            name:        "Plane Loop".to_string(),
            description: None,
            target:      target(),
            triggers:    vec![
                AutomationTrigger::Api(ApiTrigger {
                    id:      AutomationTriggerId::new("manual").unwrap(),
                    enabled: true,
                }),
                AutomationTrigger::Plane(PlaneTrigger {
                    id:                    AutomationTriggerId::new("tickets").unwrap(),
                    enabled:               true,
                    project_id:            "proj-1".to_string(),
                    ready_state_id:        "ready".to_string(),
                    in_progress_state_id:  "progress".to_string(),
                    done_state_id:         "done".to_string(),
                    cancelled_state_id:    "cancelled".to_string(),
                    failure_label_id:      Some("failed".to_string()),
                    default_harness:       ExternalAgentHarness::Omp,
                    codex_label_id:        Some("codex".to_string()),
                    omp_label_id:          Some("omp".to_string()),
                    poll_interval_seconds: 45,
                    max_concurrency:       2,
                    max_retries:           1,
                }),
            ],
        })
        .await
        .unwrap();

    let fetched = store.get(&created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    let plane = fetched.enabled_plane_triggers().next().unwrap();
    assert_eq!(plane.project_id, "proj-1");
    assert_eq!(plane.default_harness, ExternalAgentHarness::Omp);
    assert_eq!(plane.poll_interval_seconds, 45);
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
        name:        "Should roll back".to_string(),
        description: None,
        target:      target(),
        triggers:    vec![schedule("blocked", "0 7 * * *", true)],
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
    write_legacy_automation(&source_dir, "imported", "Imported").await;
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
    assert_eq!(
        store
            .get(&AutomationId::new("imported").unwrap())
            .await
            .unwrap()
            .unwrap()
            .name,
        "Imported"
    );

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

async fn write_legacy_automation(dir: &Path, id: &str, name: &str) {
    fs::write(
        dir.join(format!("{id}.toml")),
        format!(
            r#"name = "{name}"

[target]
repository = "fabro-sh/fabro"
ref = "main"
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
        ),
    )
    .await
    .unwrap();
}
