//! The SQLite run store against Petri's store contract: the conformance
//! suite, the operator release, lease exclusivity, a crash between appends,
//! and blob interoperation with Fabro's own blob store.

use std::mem;
use std::path::Path;
use std::sync::Arc;

use fabro_db::Database;
use fabro_petri::SqliteRunStore;
use fabro_store::{BlobStore, test_support};
use fabro_types::BlobHash;
use petri_store::{Access, Digest, LogId, OwnerId, RunKey, RunStore, StoreError};
use petri_testkit::run_store::{self, conformance, stale_owner_conformance};
use tokio::runtime::Handle;
use tokio::task;

/// A store over a fresh in-memory database with the production blob and
/// Petri record schemas.
fn fresh_in_memory() -> Arc<dyn RunStore> {
    Arc::new(SqliteRunStore::new(test_support::in_memory_pool_with(&[
        fabro_db::BLOBS_MIGRATION_SQL,
        fabro_db::PETRI_RECORDS_MIGRATION_SQL,
    ])))
}

/// A migrated database file, as the server opens it.
async fn migrated(path: &Path) -> Database {
    let database = Database::connect(path).await.expect("the database opens");
    database.migrate().await.expect("the migrations run");
    database
}

#[tokio::test]
async fn the_sqlite_store_passes_the_conformance_suite() {
    conformance(fresh_in_memory).await;
}

/// The operator release ends the lease from outside: the old owner's handle
/// turns stale and the next writer takes the run. The release is async, so
/// the suite's synchronous closure blocks on it in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_release_makes_the_old_owner_stale() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let database = migrated(&dir.path().join("fabro.sqlite3")).await;
    let store = SqliteRunStore::new(database.clone_pool());
    let release = |key: &RunKey| {
        task::block_in_place(|| Handle::current().block_on(store.release_lease(key)))
            .expect("the lease releases");
    };
    stale_owner_conformance(&store, release).await;
}

/// Two owners never hold one run's lease at the same time, in either order,
/// and the store reports who holds it.
#[tokio::test]
async fn two_owners_cannot_both_hold_the_lease() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let database = migrated(&dir.path().join("fabro.sqlite3")).await;
    let store = SqliteRunStore::new(database.clone_pool());
    let key = RunKey::new("exclusive");
    let first = OwnerId::new("first");
    let second = OwnerId::new("second");

    let held = store
        .open(&key, Access::Create {
            owner: first.clone(),
        })
        .await
        .expect("the first owner creates");
    assert_eq!(store.owner(&key).await.expect("reads"), Some(first.clone()));
    let refused = store
        .open(&key, Access::Write {
            owner: second.clone(),
        })
        .await
        .err()
        .expect("the second owner is refused while the first is live");
    assert!(
        matches!(&refused, StoreError::Leased { owner, .. } if *owner == first),
        "{refused}"
    );
    assert!(
        refused.to_string().contains("fabro.sqlite3")
            && refused.to_string().contains("`exclusive`"),
        "the message names the database and the run: {refused}"
    );

    drop(held);
    let taken = store
        .open(&key, Access::Write {
            owner: second.clone(),
        })
        .await
        .expect("the second owner takes the run once the first handle drops");
    assert_eq!(store.owner(&key).await.expect("reads"), Some(second));
    let refused = store
        .open(&key, Access::Write { owner: first })
        .await
        .err()
        .expect("the first owner is refused in turn");
    assert!(matches!(refused, StoreError::Leased { .. }), "{refused}");
    drop(taken);
    assert_eq!(store.owner(&key).await.expect("reads"), None);
}

/// A worker that crashes between two appends leaves a readable prefix and a
/// lease that only an operator ends: a second process opens the file, reads
/// the first batch back intact, is refused the lease until it releases it,
/// and then continues the log past the prefix.
#[tokio::test]
async fn a_crash_between_appends_leaves_a_readable_prefix() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("fabro.sqlite3");
    let key = RunKey::new("crashed");
    let log = LogId::Execution(petri_store::ExecutionId::new(0));
    let prefix = [
        run_store::record(0, "execution.started"),
        run_store::record(1, "step.started"),
    ];

    // The worker's process: its own pool over the file.
    let worker = SqliteRunStore::new(migrated(&path).await.clone_pool());
    let handle = worker
        .open(&key, Access::Create {
            owner: OwnerId::new("worker"),
        })
        .await
        .expect("the worker creates");
    handle
        .append(&log, &prefix)
        .await
        .expect("the first batch is durable");
    // The crash: the handle never drops, so nothing releases the lease.
    mem::forget(handle);

    // The server's process: a second pool over the same file.
    let server = SqliteRunStore::new(migrated(&path).await.clone_pool());
    let reader = server
        .open(&key, Access::Read)
        .await
        .expect("a reader never blocks on the lease");
    assert_eq!(reader.read(&log).await.expect("reads"), prefix);
    let refused = server
        .open(&key, Access::Write {
            owner: OwnerId::new("resumer"),
        })
        .await
        .err()
        .expect("the crashed worker's lease does not time out");
    assert!(
        matches!(&refused, StoreError::Leased { owner, .. } if owner.as_str() == "worker"),
        "{refused}"
    );

    server
        .release_lease(&key)
        .await
        .expect("the server releases the lease it observed the worker lose");
    let resumed = server
        .open(&key, Access::Write {
            owner: OwnerId::new("resumer"),
        })
        .await
        .expect("the resumer takes the run");
    assert_eq!(resumed.read(&log).await.expect("reads"), prefix);
    let error = resumed
        .append(&log, &[run_store::record(0, "different")])
        .await
        .expect_err("the prefix cannot be rewritten");
    assert!(
        matches!(error, StoreError::Conflict { seq: 0, .. }),
        "{error}"
    );
    resumed
        .append(&log, &[run_store::record(2, "step.finished")])
        .await
        .expect("the log continues past the prefix");
    assert_eq!(resumed.read(&log).await.expect("reads").len(), 3);
}

/// Petri's blobs and Fabro's blob store are one table: a blob either side
/// writes, the other reads by the same SHA-256 hex.
#[tokio::test]
async fn blobs_interoperate_with_the_blob_store() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let database = migrated(&dir.path().join("fabro.sqlite3")).await;
    let store = SqliteRunStore::new(database.clone_pool());
    let blobs = BlobStore::new(database.clone_pool());
    let logs = store
        .open(&RunKey::new("blobs"), Access::Create {
            owner: OwnerId::new("owner"),
        })
        .await
        .expect("creates");

    let graph = br#"{"nodes":[],"edges":[]}"#;
    let digest = logs.put_blob(graph).await.expect("stores");
    assert_eq!(digest.to_hex(), BlobHash::new(graph).to_string());
    let hash: BlobHash = digest.to_hex().parse().expect("the digest is a blob hash");
    assert_eq!(
        blobs.read(&hash).await.expect("reads").as_deref(),
        Some(graph.as_slice())
    );

    let output = b"large step output";
    let hash = blobs.write(output).await.expect("stores");
    let digest: Digest = hash.to_string().parse().expect("the blob hash is a digest");
    assert_eq!(
        logs.get_blob(digest).await.expect("reads"),
        Some(output.to_vec())
    );
    assert_eq!(
        logs.get_blob(Digest::of(b"missing")).await.expect("reads"),
        None
    );
}
