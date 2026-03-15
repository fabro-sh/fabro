use anyhow::{Context, Result};
use clap::Args;
use fabro_git_storage::branchstore::BranchStore;
use fabro_git_storage::gitobj::Store;
use fabro_util::terminal::Styles;
use git2::{Oid, Repository, Signature};

use crate::git::MetadataStore;
use crate::manifest::Manifest;

use super::rewind::{
    build_timeline, find_run_id_by_prefix, load_parallel_map, parse_target, print_timeline,
    resolve_target, TimelineEntry,
};

/// Fork a workflow run from an earlier checkpoint into a new run.
#[derive(Debug, Args)]
pub struct ForkArgs {
    /// Run ID (or unambiguous prefix)
    pub run_id: String,

    /// Target checkpoint: node name, node@visit, or @ordinal (omit to fork from latest)
    pub target: Option<String>,

    /// Show the checkpoint timeline instead of forking
    #[arg(long)]
    pub list: bool,

    /// Skip pushing new branches to the remote
    #[arg(long)]
    pub no_push: bool,
}

/// Entry point for `fabro fork`.
pub fn fork_command(args: &ForkArgs, styles: &Styles) -> Result<()> {
    let repo = Repository::discover(".").context("not in a git repository")?;
    let run_id = find_run_id_by_prefix(&repo, &args.run_id)?;
    let store = Store::new(repo);

    let timeline = build_timeline(&store, &run_id)?;

    if args.list {
        let parallel_map = load_parallel_map(&store, &run_id);
        print_timeline(&timeline, &parallel_map, styles);
        return Ok(());
    }

    let entry = if let Some(target_str) = &args.target {
        let target = parse_target(target_str)?;
        let parallel_map = load_parallel_map(&store, &run_id);
        resolve_target(&timeline, &target, &parallel_map)?
    } else {
        timeline
            .last()
            .ok_or_else(|| anyhow::anyhow!("no checkpoints found for run {run_id}"))?
    };

    let new_run_id = execute_fork(&store, &run_id, entry, !args.no_push)?;

    eprintln!(
        "\nForked run {} -> {}",
        &run_id[..8.min(run_id.len())],
        &new_run_id[..8.min(new_run_id.len())]
    );
    eprintln!(
        "To resume: fabro run --run-branch {}{}",
        crate::git::RUN_BRANCH_PREFIX,
        new_run_id
    );

    Ok(())
}

/// Create a new run that branches from an existing run at a specific checkpoint.
///
/// Returns the new run ID.
pub fn execute_fork(
    store: &Store,
    source_run_id: &str,
    entry: &TimelineEntry,
    push: bool,
) -> Result<String> {
    let new_run_id = ulid::Ulid::new().to_string();
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;

    // 1. Create new run branch pointing at the target checkpoint's run commit
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

    // 2. Create new metadata branch
    let source_meta_branch = MetadataStore::branch_name(source_run_id);
    let new_meta_branch = MetadataStore::branch_name(&new_run_id);
    let source_bs = BranchStore::new(store, &source_meta_branch, &sig);
    let new_bs = BranchStore::new(store, &new_meta_branch, &sig);

    new_bs
        .ensure_branch()
        .map_err(|e| anyhow::anyhow!("failed to create metadata branch: {e}"))?;

    // Read manifest, graph, and sandbox from source in a single tree lookup
    let source_entries = source_bs
        .read_entries(&["manifest.json", "graph.fabro", "sandbox.json"])
        .map_err(|e| anyhow::anyhow!("failed to read source metadata: {e}"))?;

    let mut manifest_bytes = None;
    let mut graph_bytes = None;
    let mut sandbox_bytes = None;
    for (path, data) in source_entries {
        match path {
            "manifest.json" => manifest_bytes = Some(data),
            "graph.fabro" => graph_bytes = Some(data),
            "sandbox.json" => sandbox_bytes = Some(data),
            _ => {}
        }
    }
    let manifest_bytes =
        manifest_bytes.ok_or_else(|| anyhow::anyhow!("source run has no manifest.json"))?;
    let graph_bytes =
        graph_bytes.ok_or_else(|| anyhow::anyhow!("source run has no graph.fabro"))?;

    let mut manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("failed to parse source manifest.json")?;
    manifest.run_id = new_run_id.clone();
    manifest.run_branch = Some(new_run_branch.clone());
    manifest.start_time = chrono::Utc::now();
    let new_manifest_bytes =
        serde_json::to_vec_pretty(&manifest).context("failed to serialize new manifest")?;

    // Read checkpoint from the target metadata commit (not branch tip)
    let checkpoint_bytes = store
        .read_blob_at(entry.metadata_commit_oid, "checkpoint.json")
        .map_err(|e| anyhow::anyhow!("failed to read checkpoint blob: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no checkpoint.json at metadata commit {}",
                entry.metadata_commit_oid
            )
        })?;

    // Write all entries to the new metadata branch in a single commit
    let mut file_entries: Vec<(&str, &[u8])> = vec![
        ("manifest.json", &new_manifest_bytes),
        ("graph.fabro", &graph_bytes),
        ("checkpoint.json", &checkpoint_bytes),
    ];
    if let Some(ref sandbox) = sandbox_bytes {
        file_entries.push(("sandbox.json", sandbox));
    }

    let commit_msg = format!("fork from {} @{}", source_run_id, entry.ordinal);
    new_bs
        .write_entries(&file_entries, &commit_msg)
        .map_err(|e| anyhow::anyhow!("failed to write metadata entries: {e}"))?;

    // 3. Optionally push both new branches to origin
    if push {
        let repo_path = store
            .repo()
            .workdir()
            .or_else(|| store.repo().path().parent())
            .unwrap_or(store.repo().path());

        // Check if the source run branch has a remote tracking ref (indicating we use a remote)
        let source_run_branch = format!("{}{source_run_id}", crate::git::RUN_BRANCH_PREFIX);
        let remote_ref = format!("refs/remotes/origin/{source_run_branch}");
        let has_remote_tracking = store.repo().find_reference(&remote_ref).is_ok();

        if has_remote_tracking {
            eprintln!("Pushing new branches to origin...");

            // Push run branch
            let run_refspec = format!("refs/heads/{new_run_branch}:refs/heads/{new_run_branch}");
            crate::git::push_branch(repo_path, "origin", &run_refspec)
                .map_err(|e| anyhow::anyhow!("failed to push run branch: {e}"))?;

            // Push metadata branch
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

    fn make_manifest_json(run_id: &str) -> Vec<u8> {
        let manifest = serde_json::json!({
            "run_id": run_id,
            "workflow_name": "test_workflow",
            "goal": "Test goal",
            "start_time": "2025-01-01T00:00:00Z",
            "node_count": 3,
            "edge_count": 2,
        });
        serde_json::to_vec_pretty(&manifest).unwrap()
    }

    /// Set up a source run with the given number of checkpoints.
    /// Returns (run_id, vec of run commit OIDs).
    fn setup_source_run(store: &Store, run_id: &str, nodes: &[&str]) -> Vec<Oid> {
        let sig = test_sig();

        // Create run branch with commits
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

        // Create metadata branch
        let meta_branch = MetadataStore::branch_name(run_id);
        let bs = BranchStore::new(store, &meta_branch, &sig);
        bs.ensure_branch().unwrap();

        // Write manifest and graph
        let manifest = make_manifest_json(run_id);
        let graph = b"digraph { start -> build -> test }";
        bs.write_entries(
            &[("manifest.json", &manifest), ("graph.fabro", graph)],
            "init run",
        )
        .unwrap();

        // Write checkpoint commits
        for (i, node) in nodes.iter().enumerate() {
            let cp = make_checkpoint_json(node, 1, Some(&run_oids[i].to_string()));
            bs.write_entry("checkpoint.json", &cp, "checkpoint")
                .unwrap();
        }

        run_oids
    }

    #[test]
    fn fork_creates_new_run_branch() {
        let (_dir, store) = temp_repo();
        let run_oids = setup_source_run(&store, "source-run", &["start", "build"]);

        let timeline = build_timeline(&store, "source-run").unwrap();
        // Fork at @1 (start)
        let entry = &timeline[0];

        let new_run_id = execute_fork(&store, "source-run", entry, false).unwrap();

        // Verify new run branch exists and points at the target run commit
        let new_run_branch = format!("{}{new_run_id}", crate::git::RUN_BRANCH_PREFIX);
        let resolved = store.resolve_ref(&new_run_branch).unwrap().unwrap();
        assert_eq!(resolved, run_oids[0]);
    }

    #[test]
    fn fork_creates_new_metadata_branch() {
        let (_dir, store) = temp_repo();
        setup_source_run(&store, "source-run", &["start", "build"]);

        let timeline = build_timeline(&store, "source-run").unwrap();
        let entry = &timeline[0]; // @1

        let new_run_id = execute_fork(&store, "source-run", entry, false).unwrap();

        // Verify new metadata branch exists
        let new_meta_branch = MetadataStore::branch_name(&new_run_id);
        let sig = test_sig();
        let bs = BranchStore::new(&store, &new_meta_branch, &sig);

        // Check manifest has new run_id
        let manifest_bytes = bs.read_entry("manifest.json").unwrap().unwrap();
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
        assert_eq!(manifest.run_id, new_run_id);
        assert_eq!(
            manifest.run_branch.as_deref(),
            Some(format!("{}{new_run_id}", crate::git::RUN_BRANCH_PREFIX).as_str())
        );

        // Check graph exists
        let graph_bytes = bs.read_entry("graph.fabro").unwrap().unwrap();
        assert_eq!(graph_bytes, b"digraph { start -> build -> test }");

        // Check checkpoint matches target (@1 = start)
        let cp_bytes = bs.read_entry("checkpoint.json").unwrap().unwrap();
        let cp: serde_json::Value = serde_json::from_slice(&cp_bytes).unwrap();
        assert_eq!(cp["current_node"], "start");
    }

    #[test]
    fn fork_preserves_original_run() {
        let (_dir, store) = temp_repo();
        let run_oids = setup_source_run(&store, "source-run", &["start", "build", "test"]);

        // Record original refs
        let source_run_branch = format!("{}source-run", crate::git::RUN_BRANCH_PREFIX);
        let source_meta_branch = MetadataStore::branch_name("source-run");
        let original_run_ref = store.resolve_ref(&source_run_branch).unwrap().unwrap();
        let original_meta_ref = store.resolve_ref(&source_meta_branch).unwrap().unwrap();

        let timeline = build_timeline(&store, "source-run").unwrap();
        let entry = &timeline[0]; // @1

        execute_fork(&store, "source-run", entry, false).unwrap();

        // Verify source branches are untouched
        let after_run_ref = store.resolve_ref(&source_run_branch).unwrap().unwrap();
        let after_meta_ref = store.resolve_ref(&source_meta_branch).unwrap().unwrap();
        assert_eq!(original_run_ref, after_run_ref);
        assert_eq!(original_meta_ref, after_meta_ref);

        // Verify source run branch still points at the last commit (test)
        assert_eq!(after_run_ref, run_oids[2]);
    }

    #[test]
    fn fork_defaults_to_latest_checkpoint() {
        let (_dir, store) = temp_repo();
        let run_oids = setup_source_run(&store, "source-run", &["start", "build", "test"]);

        let repo = store.repo();
        let run_id = find_run_id_by_prefix(repo, "source-run").unwrap();
        let timeline = build_timeline(&store, &run_id).unwrap();

        // Default: fork from the last checkpoint
        let entry = timeline.last().unwrap();
        let new_run_id = execute_fork(&store, &run_id, entry, false).unwrap();

        // Verify new run branch points at the last run commit (test)
        let new_run_branch = format!("{}{new_run_id}", crate::git::RUN_BRANCH_PREFIX);
        let resolved = store.resolve_ref(&new_run_branch).unwrap().unwrap();
        assert_eq!(resolved, run_oids[2]);
    }

    #[test]
    fn fork_at_specific_ordinal() {
        let (_dir, store) = temp_repo();
        let run_oids = setup_source_run(&store, "source-run", &["start", "build", "test"]);

        let timeline = build_timeline(&store, "source-run").unwrap();

        // Fork at @2 (build)
        let target = parse_target("@2").unwrap();
        let entry = resolve_target(&timeline, &target, &HashMap::new()).unwrap();
        let new_run_id = execute_fork(&store, "source-run", entry, false).unwrap();

        // Verify new run branch points at the second run commit (build)
        let new_run_branch = format!("{}{new_run_id}", crate::git::RUN_BRANCH_PREFIX);
        let resolved = store.resolve_ref(&new_run_branch).unwrap().unwrap();
        assert_eq!(resolved, run_oids[1]);
    }
}
