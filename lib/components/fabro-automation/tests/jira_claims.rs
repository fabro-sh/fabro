#![expect(
    clippy::unwrap_used,
    reason = "SQLite claim-store integration tests use panic-on-failure fixture setup"
)]

use chrono::{DateTime, Utc};
use fabro_automation::{AutomationId, JiraIssueClaim, JiraIssueRunStore};
use fabro_db::Database;
use fabro_types::RunId;

async fn test_store() -> (tempfile::TempDir, JiraIssueRunStore) {
    let dir = tempfile::tempdir().unwrap();
    let database = Database::connect(dir.path().join("fabro.sqlite3"))
        .await
        .unwrap();
    database.migrate().await.unwrap();
    (dir, JiraIssueRunStore::new(database.clone_pool()))
}

fn id(value: &str) -> AutomationId {
    AutomationId::new(value).unwrap()
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value).unwrap().to_utc()
}

#[tokio::test]
async fn claim_is_idempotent_per_automation_and_key() {
    let (_dir, store) = test_store().await;
    let now = at("2026-11-08T10:00:00Z");

    assert!(
        store
            .claim(&id("a"), "FAB-1", RunId::new(), now)
            .await
            .unwrap()
    );
    assert!(
        !store
            .claim(&id("a"), "FAB-1", RunId::new(), now)
            .await
            .unwrap()
    );
    assert!(
        store
            .claim(&id("b"), "FAB-1", RunId::new(), now)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn release_returns_whether_a_row_existed() {
    let (_dir, store) = test_store().await;
    let now = at("2026-11-08T10:00:00Z");
    store
        .claim(&id("a"), "FAB-1", RunId::new(), now)
        .await
        .unwrap();

    assert!(store.release(&id("a"), "FAB-1").await.unwrap());
    assert!(!store.release(&id("a"), "FAB-1").await.unwrap());
}

#[tokio::test]
async fn by_run_finds_the_claim() {
    let (_dir, store) = test_store().await;
    let now = at("2026-11-08T10:00:00Z");
    let run_id = RunId::new();
    store.claim(&id("a"), "FAB-1", run_id, now).await.unwrap();

    assert_eq!(
        store.by_run(run_id).await.unwrap(),
        Some(JiraIssueClaim {
            automation_id: id("a"),
            issue_key: "FAB-1".to_string(),
            run_id,
            created_at: now,
        })
    );
    assert_eq!(store.by_run(RunId::new()).await.unwrap(), None);
}

#[tokio::test]
async fn claimed_keys_lists_only_that_automation() {
    let (_dir, store) = test_store().await;
    let now = at("2026-11-08T10:00:00Z");
    store
        .claim(&id("a"), "FAB-2", RunId::new(), now)
        .await
        .unwrap();
    store
        .claim(&id("a"), "FAB-1", RunId::new(), now)
        .await
        .unwrap();
    store
        .claim(&id("b"), "FAB-9", RunId::new(), now)
        .await
        .unwrap();

    let keys = store.claimed_keys(&id("a")).await.unwrap();
    assert_eq!(
        keys,
        ["FAB-1", "FAB-2"].map(String::from).into_iter().collect()
    );
    let listed: Vec<String> = store
        .list(&id("a"))
        .await
        .unwrap()
        .into_iter()
        .map(|claim| claim.issue_key)
        .collect();
    assert_eq!(listed, ["FAB-1", "FAB-2"]);
}

#[tokio::test]
async fn unstarted_listing_skips_marked_and_recent_claims() {
    let (_dir, store) = test_store().await;
    let old = at("2026-11-08T10:00:00Z");
    let cutoff = at("2026-11-08T10:05:00Z");
    let a = id("a");
    store.claim(&a, "FAB-1", RunId::new(), old).await.unwrap();
    store.claim(&a, "FAB-2", RunId::new(), old).await.unwrap();
    store
        .claim(&a, "FAB-3", RunId::new(), cutoff)
        .await
        .unwrap();
    assert!(store.mark_run_created(&a, "FAB-2", old).await.unwrap());
    assert!(!store.mark_run_created(&a, "FAB-9", old).await.unwrap());

    let keys: Vec<_> = store
        .list_unstarted_before(&a, cutoff)
        .await
        .unwrap()
        .into_iter()
        .map(|claim| claim.issue_key)
        .collect();
    assert_eq!(keys, ["FAB-1"]);
}
