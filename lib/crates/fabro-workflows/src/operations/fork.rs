use anyhow::{Context, Result};
use fabro_git_storage::branchstore::BranchStore;
use fabro_git_storage::gitobj::Store;
use git2::{Oid, Signature};

use crate::git::{MetadataStore, RUN_BRANCH_PREFIX, push_run_branches};
use crate::records::RunRecord;
use crate::records::StartRecord;

use super::rewind::{RewindTarget, TimelineEntry, build_timeline};

#[derive(Debug, Clone)]
pub struct ForkRunInput {
    pub source_run_id: String,
    pub target: Option<RewindTarget>,
    pub push: bool,
}

/// Create a new run that branches from an existing run at a specific checkpoint.
///
/// Returns the new run ID.
pub fn fork(store: &Store, input: ForkRunInput) -> Result<String> {
    let timeline = build_timeline(store, &input.source_run_id)?;
    let entry = match input.target.as_ref() {
        Some(target) => timeline.resolve(target)?,
        None => timeline.entries.last().ok_or_else(|| {
            anyhow::anyhow!("no checkpoints found for run {}", input.source_run_id)
        })?,
    };
    fork_from_entry(store, &input.source_run_id, entry, input.push)
}

fn fork_from_entry(
    store: &Store,
    source_run_id: &str,
    entry: &TimelineEntry,
    push: bool,
) -> Result<String> {
    let new_run_id = ulid::Ulid::new().to_string();
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;

    let new_run_branch = format!("{}{new_run_id}", RUN_BRANCH_PREFIX);
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
        let source_run_branch = format!("{}{source_run_id}", RUN_BRANCH_PREFIX);
        let run_refspec = format!("refs/heads/{new_run_branch}:refs/heads/{new_run_branch}");
        let meta_refspec = format!("refs/heads/{new_meta_branch}:refs/heads/{new_meta_branch}");
        push_run_branches(
            store,
            &source_run_branch,
            Some(&run_refspec),
            &meta_refspec,
            "new",
        )?;
    }

    Ok(new_run_id)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::super::test_support::*;
    use super::*;
    use git2::Oid;

    use crate::operations::find_run_id_by_prefix;

    fn make_run_record_json(run_id: &str) -> Vec<u8> {
        let record = serde_json::json!({
            "run_id": run_id,
            "created_at": "2025-01-01T00:00:00Z",
            "settings": {},
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
            "run_branch": format!("{}{}", RUN_BRANCH_PREFIX, run_id),
        });
        serde_json::to_vec_pretty(&record).unwrap()
    }

    fn setup_source_run(store: &Store, run_id: &str, nodes: &[&str]) -> Vec<Oid> {
        let sig = test_sig();

        let run_branch = format!("{}{run_id}", RUN_BRANCH_PREFIX);
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

        let new_run_id = fork(
            &store,
            ForkRunInput {
                source_run_id: source_run_id.to_string(),
                target: Some(RewindTarget::from_str("@2").unwrap()),
                push: false,
            },
        )
        .unwrap();

        let new_run_branch = format!("{}{new_run_id}", RUN_BRANCH_PREFIX);
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

        let err = fork_from_entry(&store, run_id, &entry, false)
            .unwrap_err()
            .to_string();
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
