//! A large stage value leaves the run's records for Fabro's blob table
//! under `blob://sha256/<hex>`, and comes back from the same table.
//!
//! The run takes its host scope through the sandbox-driver host plugin, so
//! the test skips, and says why, when the executable is not found, unless
//! `FABRO_REQUIRE_SANDBOX_PLUGINS` is set.

mod support;

use std::sync::Arc;

use fabro_petri::SqliteRunStore;
use fabro_petri::blobs::Blobs;
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::runtime::RuntimeSpec;
use fabro_store::{BlobStore, test_support};
use fabro_types::BlobHash;
use petri_attractor_steps::blobs::{BLOB_REF_PREFIX, OFFLOAD_THRESHOLD, parse_blob_ref};
use support::{SETTINGS, Silent, admit, all_records, host_plugin, no_questions, run_request};

/// One line of the command's output.
const LINE: &str = "xxxxxxxx";

/// The command prints `lines` lines, more than the offload threshold in
/// all.
fn workflow(lines: usize) -> String {
    format!(
        r#"digraph Big {{
    graph [goal="Print a lot"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="yes {LINE} | head -n {lines}"]
    start -> say -> exit
}}"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_output_round_trips_through_the_blob_table() {
    if host_plugin().is_none() {
        return;
    }
    let root = tempfile::tempdir().expect("a temp dir");
    let pool = test_support::in_memory_pool_with(&[
        fabro_db::BLOBS_MIGRATION_SQL,
        fabro_db::PETRI_RECORDS_MIGRATION_SQL,
    ]);
    let store = Arc::new(SqliteRunStore::new(pool.clone()));
    let blobs = Arc::new(BlobStore::new(pool.clone()));
    let lines = OFFLOAD_THRESHOLD / (LINE.len() + 1) + 512;
    let expected = format!("{LINE}\n").repeat(lines);
    let workflow = workflow(lines);
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let mut request = run_request(
        "big",
        &root.path().join("run"),
        graphs,
        store.clone(),
        runtime,
        no_questions(Arc::new(Silent)),
    );
    request.blobs = Some(blobs.clone());

    let outcome = engine::run(request).await.expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let records = all_records(store.as_ref(), "big").await;
    let rendered: Vec<String> = records.iter().map(ToString::to_string).collect();
    let inline = serde_json::to_string(&expected).expect("encodes");
    let inline = inline.trim_matches('"');
    assert!(
        rendered.iter().all(|record| !record.contains(inline)),
        "the output stayed inline in a record"
    );
    let reference = rendered
        .iter()
        .find_map(|record| {
            let start = record.find(BLOB_REF_PREFIX)?;
            let tail = &record[start..];
            let end = tail.find(['"', '#']).unwrap_or(tail.len());
            Some(tail[..end].to_string())
        })
        .expect("a record carries the reference");
    let digest = parse_blob_ref(&reference).expect("a well-formed reference");
    let hash: BlobHash = digest.parse().expect("a blob hash");
    let bytes = Blobs::read(blobs.as_ref(), &hash)
        .await
        .expect("the table reads")
        .expect("the blob is in the table");
    assert_eq!(
        String::from_utf8(bytes.to_vec()).expect("text"),
        expected,
        "the blob is the output byte for byte"
    );
    assert_eq!(BlobHash::new(&bytes), hash, "content-addressed");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs")
        .fetch_one(&pool)
        .await
        .expect("the blob table counts");
    assert!(count >= 1, "the blob is a row of Fabro's table");
    // The run directory's own store was not used: nothing under it holds
    // the digest.
    let local = root.path().join("run").join("blobs").join(digest);
    assert!(!local.exists(), "the local store was bypassed");
}
