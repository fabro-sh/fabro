use anyhow::{Context, Result};
use fabro_checkpoint::branch::BranchStore;
use fabro_checkpoint::git::Store;
use fabro_store::RunProjection;
use fabro_types::RunId;
use git2::{Oid, Signature};

use super::rewind::{RewindTarget, TimelineEntry, build_timeline};
use crate::git::{MetadataStore, RUN_BRANCH_PREFIX, push_run_branches};
use crate::records::{Checkpoint, RunSpec, StartRecord};
use crate::run_dump::RunDump;

#[derive(Debug, Clone)]
pub struct ForkRunInput {
    pub source_run_id: RunId,
    pub target:        Option<RewindTarget>,
    pub push:          bool,
}

/// Create a new run that branches from an existing run at a specific
/// checkpoint.
///
/// Returns the new run ID.
pub fn fork(store: &Store, input: &ForkRunInput) -> Result<RunId> {
    let timeline = build_timeline(store, &input.source_run_id.to_string())?;
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
    source_run_id: &RunId,
    entry: &TimelineEntry,
    push: bool,
) -> Result<RunId> {
    let new_run_id = RunId::new();
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;

    let new_run_branch = format!("{RUN_BRANCH_PREFIX}{new_run_id}");
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

    let source_meta_branch = MetadataStore::branch_name(&source_run_id.to_string());
    let new_meta_branch = MetadataStore::branch_name(&new_run_id.to_string());
    let source_bs = BranchStore::new(store, &source_meta_branch, &sig);
    let new_bs = BranchStore::new(store, &new_meta_branch, &sig);

    new_bs
        .ensure_branch()
        .map_err(|e| anyhow::anyhow!("failed to create metadata branch: {e}"))?;

    let source_projection = source_bs
        .read_entry("run.json")
        .map_err(|e| anyhow::anyhow!("failed to read source metadata: {e}"))?
        .context("source run has no run.json")
        .and_then(|bytes| {
            serde_json::from_slice::<RunProjection>(&bytes)
                .context("failed to parse source run.json")
        })?;

    let mut run_spec: RunSpec = source_projection
        .spec
        .clone()
        .context("source run projection has no spec")?;
    run_spec.run_id = new_run_id;

    let start_record = StartRecord {
        run_id:     new_run_id,
        start_time: new_run_id.created_at(),
        run_branch: Some(new_run_branch.clone()),
        base_sha:   None,
    };

    let mut init_projection = RunProjection::default();
    init_projection.spec = Some(run_spec.clone());
    init_projection
        .graph_source
        .clone_from(&source_projection.graph_source);
    init_projection.start = Some(start_record.clone());
    init_projection
        .sandbox
        .clone_from(&source_projection.sandbox);

    let checkpoint_bytes = store
        .read_blob_at(entry.metadata_commit_oid, "run.json")
        .map_err(|e| anyhow::anyhow!("failed to read checkpoint snapshot: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no run.json at metadata commit {}",
                entry.metadata_commit_oid
            )
        })?;
    let mut checkpoint_projection: RunProjection = serde_json::from_slice(&checkpoint_bytes)
        .context("failed to parse source checkpoint snapshot")?;
    let mut checkpoint: Checkpoint = checkpoint_projection
        .checkpoint
        .clone()
        .context("source checkpoint snapshot has no checkpoint")?;
    checkpoint.git_commit_sha.clone_from(&entry.run_commit_sha);
    checkpoint_projection.spec = Some(run_spec);
    checkpoint_projection.graph_source = source_projection.graph_source;
    checkpoint_projection.start = Some(start_record);
    checkpoint_projection.sandbox = source_projection.sandbox;
    checkpoint_projection.checkpoint = Some(checkpoint);

    let init_entries = RunDump::from_projection(&init_projection)
        .git_entries()
        .context("failed to build init metadata snapshot")?;
    let init_refs: Vec<(&str, &[u8])> = init_entries
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    new_bs
        .write_entries(&init_refs, "init run")
        .map_err(|e| anyhow::anyhow!("failed to write init metadata snapshot: {e}"))?;

    let checkpoint_entries = RunDump::from_projection(&checkpoint_projection)
        .git_entries()
        .context("failed to build checkpoint metadata snapshot")?;
    let checkpoint_refs: Vec<(&str, &[u8])> = checkpoint_entries
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    new_bs
        .write_entries(&checkpoint_refs, "checkpoint")
        .map_err(|e| anyhow::anyhow!("failed to write metadata snapshot: {e}"))?;

    if push {
        let source_run_branch = format!("{RUN_BRANCH_PREFIX}{source_run_id}");
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

    use fabro_store::RunProjection;
    use fabro_types::RunId;
    use git2::Oid;

    use super::super::test_support::*;
    use super::*;
    use crate::operations::find_run_id_by_prefix;

    fn parse_run_id(value: &str) -> RunId {
        value.parse().unwrap()
    }

    fn make_run_projection(run_id: &RunId) -> RunProjection {
        let mut projection = RunProjection::default();
        projection.spec = Some(
            serde_json::from_value(serde_json::json!({
                "run_id": run_id.to_string(),
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
            }))
            .unwrap(),
        );
        projection
    }

    fn make_start_record_json(run_id: &RunId) -> Vec<u8> {
        let record = serde_json::json!({
            "run_id": run_id.to_string(),
            "start_time": "2025-01-01T00:00:00Z",
            "run_branch": format!("{}{}", RUN_BRANCH_PREFIX, run_id),
        });
        serde_json::to_vec_pretty(&record).unwrap()
    }

    fn setup_source_run(store: &Store, run_id: &RunId, nodes: &[&str]) -> Vec<Oid> {
        let sig = test_sig();

        let run_branch = format!("{RUN_BRANCH_PREFIX}{run_id}");
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

        let meta_branch = MetadataStore::branch_name(&run_id.to_string());
        let bs = BranchStore::new(store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();

        let mut init_projection = make_run_projection(run_id);
        init_projection.start =
            Some(serde_json::from_slice(&make_start_record_json(run_id)).unwrap());
        let init_json = serde_json::to_vec_pretty(&init_projection).unwrap();
        bs.write_entries(&[("run.json", &init_json)], "init run")
            .unwrap();

        for (i, node) in nodes.iter().enumerate() {
            let mut projection = init_projection.clone();
            projection.checkpoint = Some(
                serde_json::from_slice(&make_checkpoint_bytes(
                    node,
                    1,
                    Some(&run_oids[i].to_string()),
                ))
                .unwrap(),
            );
            let projection_json = serde_json::to_vec_pretty(&projection).unwrap();
            bs.write_entry("run.json", &projection_json, "checkpoint")
                .unwrap();
        }

        run_oids
    }

    #[test]
    fn fork_creates_new_run_and_metadata_branches() {
        let (_dir, store) = temp_repo();
        let source_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let run_oids = setup_source_run(&store, &source_run_id, &["start", "build", "test"]);

        let new_run_id = fork(&store, &ForkRunInput {
            source_run_id,
            target: Some(RewindTarget::from_str("@2").unwrap()),
            push: false,
        })
        .unwrap();

        let new_run_branch = format!("{RUN_BRANCH_PREFIX}{new_run_id}");
        let new_meta_branch = MetadataStore::branch_name(&new_run_id.to_string());

        assert!(store.resolve_ref(&new_run_branch).unwrap().is_some());
        assert!(store.resolve_ref(&new_meta_branch).unwrap().is_some());

        let sig = test_sig();
        let bs = BranchStore::new(&store, &new_meta_branch, &sig);
        let run_json = bs.read_entry("run.json").unwrap().unwrap();
        let run_spec: RunProjection = serde_json::from_slice(&run_json).unwrap();
        assert_eq!(
            run_spec.spec.as_ref().map(|run| run.run_id),
            Some(new_run_id)
        );

        let timeline = build_timeline(&store, &new_run_id.to_string()).unwrap();
        assert_eq!(timeline.entries.len(), 1);
        assert_eq!(timeline.entries[0].node_name, "build");
        assert_eq!(
            timeline.entries[0].run_commit_sha,
            Some(run_oids[1].to_string())
        );
    }

    #[test]
    fn fork_rejects_checkpoint_without_run_sha() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAW");
        let meta_branch = MetadataStore::branch_name(&run_id.to_string());
        let bs = BranchStore::new(&store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();
        let init_projection = serde_json::to_vec_pretty(&make_run_projection(&run_id)).unwrap();
        bs.write_entry("run.json", &init_projection, "init")
            .unwrap();

        let mut checkpoint_projection = make_run_projection(&run_id);
        checkpoint_projection.checkpoint =
            Some(serde_json::from_slice(&make_checkpoint_bytes("start", 1, None)).unwrap());
        let cp = serde_json::to_vec_pretty(&checkpoint_projection).unwrap();
        let oid = bs.write_entry("run.json", &cp, "checkpoint").unwrap();
        let entry = TimelineEntry {
            ordinal:             1,
            node_name:           "start".to_string(),
            visit:               1,
            metadata_commit_oid: oid,
            run_commit_sha:      None,
        };

        let err = fork_from_entry(&store, &run_id, &entry, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot fork"));
    }

    #[test]
    fn fork_supports_prefix_resolved_source_run_ids() {
        let (_dir, store) = temp_repo();
        let source_run_id = parse_run_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
        setup_source_run(&store, &source_run_id, &["start", "build"]);

        let resolved = find_run_id_by_prefix(store.repo(), "01ARZ3").unwrap();
        assert_eq!(resolved, source_run_id);
    }
}
