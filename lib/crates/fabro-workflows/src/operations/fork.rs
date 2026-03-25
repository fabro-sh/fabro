use anyhow::{Context, Result};
use fabro_git_storage::branchstore::BranchStore;
use fabro_git_storage::gitobj::Store;
use git2::{Oid, Signature};

use crate::git::MetadataStore;
use crate::records::RunRecord;
use crate::records::StartRecord;

use super::rewind::TimelineEntry;

/// Create a new run that branches from an existing run at a specific checkpoint.
///
/// Returns the new run ID.
pub fn fork(
    store: &Store,
    source_run_id: &str,
    entry: &TimelineEntry,
    push: bool,
) -> Result<String> {
    let new_run_id = ulid::Ulid::new().to_string();
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;

    let new_run_branch = format!("{}{new_run_id}", crate::git::RUN_BRANCH_PREFIX);
    match &entry.run_commit_sha {
        Some(sha) => {
            let oid =
                Oid::from_str(sha).with_context(|| format!("invalid run commit SHA: {sha}"))?;
            store
                .update_ref(&new_run_branch, oid)
                .map_err(|e| anyhow::anyhow!("failed to create run branch ref: {e}"))?;
        }
        None => {
            anyhow::bail!(
                "checkpoint @{} has no git_commit_sha; cannot fork",
                entry.ordinal
            );
        }
    }

    let source_meta_branch = MetadataStore::branch_name(source_run_id);
    let new_meta_branch = MetadataStore::branch_name(&new_run_id);
    let source_bs = BranchStore::new(store, &source_meta_branch, &sig);
    let new_bs = BranchStore::new(store, &new_meta_branch, &sig);

    new_bs
        .ensure_branch()
        .map_err(|e| anyhow::anyhow!("failed to create metadata branch: {e}"))?;

    let source_entries = source_bs
        .read_entries(&["run.json", "start.json", "sandbox.json"])
        .map_err(|e| anyhow::anyhow!("failed to read source metadata: {e}"))?;

    let mut run_record_bytes = None;
    let mut start_record_bytes = None;
    let mut sandbox_bytes = None;
    for (path, data) in source_entries {
        match path {
            "run.json" => run_record_bytes = Some(data),
            "start.json" => start_record_bytes = Some(data),
            "sandbox.json" => sandbox_bytes = Some(data),
            _ => {}
        }
    }
    let run_record_bytes =
        run_record_bytes.ok_or_else(|| anyhow::anyhow!("source run has no run.json"))?;

    let now = chrono::Utc::now();

    let mut run_record: RunRecord =
        serde_json::from_slice(&run_record_bytes).context("failed to parse source run.json")?;
    run_record.run_id = new_run_id.clone();
    run_record.created_at = now;
    let new_run_record_bytes =
        serde_json::to_vec_pretty(&run_record).context("failed to serialize new run.json")?;

    let new_start_record_bytes = if start_record_bytes.is_some() {
        let start_record = StartRecord {
            run_id: new_run_id.clone(),
            start_time: now,
            run_branch: Some(new_run_branch.clone()),
            base_sha: None,
        };
        Some(
            serde_json::to_vec_pretty(&start_record)
                .context("failed to serialize new start.json")?,
        )
    } else {
        None
    };

    let checkpoint_bytes = store
        .read_blob_at(entry.metadata_commit_oid, "checkpoint.json")
        .map_err(|e| anyhow::anyhow!("failed to read checkpoint blob: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no checkpoint.json at metadata commit {}",
                entry.metadata_commit_oid
            )
        })?;

    let mut file_entries: Vec<(&str, &[u8])> = vec![
        ("run.json", &new_run_record_bytes),
        ("checkpoint.json", &checkpoint_bytes),
    ];
    if let Some(ref start_record) = new_start_record_bytes {
        file_entries.push(("start.json", start_record));
    }
    if let Some(ref sandbox) = sandbox_bytes {
        file_entries.push(("sandbox.json", sandbox));
    }

    let commit_msg = format!("fork from {} @{}", source_run_id, entry.ordinal);
    new_bs
        .write_entries(&file_entries, &commit_msg)
        .map_err(|e| anyhow::anyhow!("failed to write metadata entries: {e}"))?;

    if push {
        let repo_path = store
            .repo()
            .workdir()
            .or_else(|| store.repo().path().parent())
            .unwrap_or(store.repo().path());

        let source_run_branch = format!("{}{source_run_id}", crate::git::RUN_BRANCH_PREFIX);
        let remote_ref = format!("refs/remotes/origin/{source_run_branch}");
        let has_remote_tracking = store.repo().find_reference(&remote_ref).is_ok();

        if has_remote_tracking {
            eprintln!("Pushing new branches to origin...");

            let run_refspec = format!("refs/heads/{new_run_branch}:refs/heads/{new_run_branch}");
            crate::git::push_branch(repo_path, "origin", &run_refspec)
                .map_err(|e| anyhow::anyhow!("failed to push run branch: {e}"))?;

            let meta_refspec = format!("refs/heads/{new_meta_branch}:refs/heads/{new_meta_branch}");
            crate::git::push_branch(repo_path, "origin", &meta_refspec)
                .map_err(|e| anyhow::anyhow!("failed to push metadata branch: {e}"))?;

            eprintln!("Remote refs updated.");
        }
    }

    Ok(new_run_id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use git2::Repository;

    use crate::operations::{build_timeline, find_run_id_by_prefix, parse_target, resolve_target};

    fn temp_repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, Store::new(repo))
    }

    fn test_sig() -> Signature<'static> {
        Signature::now("Test", "test@example.com").unwrap()
    }

    fn make_checkpoint_json(current_node: &str, visit: usize, git_sha: Option<&str>) -> Vec<u8> {
        let mut node_visits = HashMap::new();
        node_visits.insert(current_node.to_string(), visit);
        let cp = serde_json::json!({
            "timestamp": "2025-01-01T00:00:00Z",
            "current_node": current_node,
            "completed_nodes": [current_node],
            "node_retries": {},
            "context_values": {},
            "logs": [],
            "node_visits": node_visits,
            "git_commit_sha": git_sha,
        });
        serde_json::to_vec(&cp).unwrap()
    }

    fn make_run_record_json(run_id: &str) -> Vec<u8> {
        let record = serde_json::json!({
            "run_id": run_id,
            "created_at": "2025-01-01T00:00:00Z",
            "config": {},
            "graph": {
                "name": "test_workflow",
                "nodes": {
                    "start": {"id": "start", "attrs": {}},
                    "build": {"id": "build", "attrs": {}},
                    "test": {"id": "test", "attrs": {}}
                },
                "edges": [
                    {"from": "start", "to": "build", "attrs": {}},
                    {"from": "build", "to": "test", "attrs": {}}
                ],
                "attrs": {}
            },
            "working_directory": "/tmp/test",
        });
        serde_json::to_vec_pretty(&record).unwrap()
    }

    fn make_start_record_json(run_id: &str) -> Vec<u8> {
        let record = serde_json::json!({
            "run_id": run_id,
            "start_time": "2025-01-01T00:00:00Z",
            "run_branch": format!("{}{}", crate::git::RUN_BRANCH_PREFIX, run_id),
        });
        serde_json::to_vec_pretty(&record).unwrap()
    }

    fn setup_source_run(store: &Store, run_id: &str, nodes: &[&str]) -> Vec<Oid> {
        let sig = test_sig();

        let run_branch = format!("{}{run_id}", crate::git::RUN_BRANCH_PREFIX);
        let empty_tree = store.write_empty_tree().unwrap();
        let mut run_oids = Vec::new();
        let mut parent: Option<Oid> = None;

        for node in nodes {
            let parents = match parent {
                Some(p) => vec![p],
                None => vec![],
            };
            let oid = store
                .write_commit(
                    empty_tree,
                    &parents,
                    &format!("fabro({run_id}): {node} (completed)"),
                    &sig,
                )
                .unwrap();
            store.update_ref(&run_branch, oid).unwrap();
            run_oids.push(oid);
            parent = Some(oid);
        }

        let meta_branch = MetadataStore::branch_name(run_id);
        let bs = BranchStore::new(store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();

        let run_record = make_run_record_json(run_id);
        let start_record = make_start_record_json(run_id);
        bs.write_entries(
            &[("run.json", &run_record), ("start.json", &start_record)],
            "init run",
        )
        .unwrap();

        for (i, node) in nodes.iter().enumerate() {
            let cp = make_checkpoint_json(node, 1, Some(&run_oids[i].to_string()));
            bs.write_entry("checkpoint.json", &cp, "checkpoint")
                .unwrap();
        }

        run_oids
    }

    #[test]
    fn fork_creates_new_run_and_metadata_branches() {
        let (_dir, store) = temp_repo();
        let source_run_id = "run-source";
        let _run_oids = setup_source_run(&store, source_run_id, &["start", "build", "test"]);

        let timeline = build_timeline(&store, source_run_id).unwrap();
        let entry =
            resolve_target(&timeline, &parse_target("@2").unwrap(), &HashMap::new()).unwrap();

        let new_run_id = fork(&store, source_run_id, entry, false).unwrap();

        let new_run_branch = format!("{}{new_run_id}", crate::git::RUN_BRANCH_PREFIX);
        let new_meta_branch = MetadataStore::branch_name(&new_run_id);

        assert!(store.resolve_ref(&new_run_branch).unwrap().is_some());
        assert!(store.resolve_ref(&new_meta_branch).unwrap().is_some());

        let sig = test_sig();
        let bs = BranchStore::new(&store, &new_meta_branch, &sig);
        let run_json = bs.read_entry("run.json").unwrap().unwrap();
        let run_record: RunRecord = serde_json::from_slice(&run_json).unwrap();
        assert_eq!(run_record.run_id, new_run_id);
    }

    #[test]
    fn fork_rejects_checkpoint_without_run_sha() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let run_id = "run-no-sha";
        let meta_branch = MetadataStore::branch_name(run_id);
        let bs = BranchStore::new(&store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();
        bs.write_entry("run.json", &make_run_record_json(run_id), "init")
            .unwrap();

        let cp = make_checkpoint_json("start", 1, None);
        let oid = bs
            .write_entry("checkpoint.json", &cp, "checkpoint")
            .unwrap();
        let entry = TimelineEntry {
            ordinal: 1,
            node_name: "start".to_string(),
            visit: 1,
            metadata_commit_oid: oid,
            run_commit_sha: None,
        };

        let err = fork(&store, run_id, &entry, false).unwrap_err().to_string();
        assert!(err.contains("cannot fork"));
    }

    #[test]
    fn fork_supports_prefix_resolved_source_run_ids() {
        let (_dir, store) = temp_repo();
        let source_run_id = "abc-123-long";
        setup_source_run(&store, source_run_id, &["start", "build"]);

        let resolved = find_run_id_by_prefix(store.repo(), "abc-123").unwrap();
        assert_eq!(resolved, source_run_id);
    }
}
