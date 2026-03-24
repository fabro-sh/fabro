use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use fabro_git_storage::branchstore::{BranchStore, CommitInfo};
use fabro_git_storage::gitobj::Store;
use git2::{Oid, Repository, Signature};

use crate::checkpoint::Checkpoint;
use crate::git::MetadataStore;
use fabro_graphviz::graph::Graph;

/// Parsed rewind target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindTarget {
    /// @N — the Nth checkpoint (1-based)
    Ordinal(usize),
    /// node_name — most recent visit of the named node
    LatestVisit(String),
    /// node_name@N — the Nth visit of the named node
    SpecificVisit(String, usize),
}

/// One row in the checkpoint timeline.
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    /// 1-based ordinal (checkpoint sequence number)
    pub ordinal: usize,
    /// The node that was just completed at this checkpoint
    pub node_name: String,
    /// Visit number for this node (from node_visits)
    pub visit: usize,
    /// OID of the metadata-branch commit that contains this checkpoint
    pub metadata_commit_oid: Oid,
    /// SHA of the run-branch commit captured at this checkpoint
    pub run_commit_sha: Option<String>,
}

/// Parse a target string into a `RewindTarget`.
pub fn parse_target(s: &str) -> Result<RewindTarget> {
    if let Some(rest) = s.strip_prefix('@') {
        let n: usize = rest
            .parse()
            .with_context(|| format!("invalid ordinal: @{rest}"))?;
        if n == 0 {
            bail!("ordinal must be >= 1");
        }
        return Ok(RewindTarget::Ordinal(n));
    }
    if let Some(at_pos) = s.rfind('@') {
        let name = &s[..at_pos];
        let visit_str = &s[at_pos + 1..];
        if !name.is_empty() && !visit_str.is_empty() {
            if let Ok(visit) = visit_str.parse::<usize>() {
                if visit == 0 {
                    bail!("visit number must be >= 1");
                }
                return Ok(RewindTarget::SpecificVisit(name.to_string(), visit));
            }
        }
    }
    Ok(RewindTarget::LatestVisit(s.to_string()))
}

/// Build the checkpoint timeline by walking the metadata branch oldest-first.
///
/// The metadata branch checkpoint.json may not contain `git_commit_sha` (the engine
/// only writes it to the on-disk checkpoint). As a fallback, we walk the run branch
/// and match commits by message pattern `fabro({run_id}): {node_name}`.
pub fn build_timeline(store: &Store, run_id: &str) -> Result<Vec<TimelineEntry>> {
    let branch = MetadataStore::branch_name(run_id);
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;
    let bs = BranchStore::new(store, &branch, &sig);

    let commits = bs
        .log(10_000)
        .map_err(|e| anyhow::anyhow!("failed to read metadata branch log: {e}"))?;

    // Reverse to oldest-first
    let commits: Vec<&CommitInfo> = commits.iter().rev().collect();

    let mut timeline = Vec::new();
    let mut ordinal = 0usize;

    for commit in &commits {
        if !commit.message.starts_with("checkpoint") {
            continue;
        }
        let blob = store
            .read_blob_at(commit.oid, "checkpoint.json")
            .map_err(|e| anyhow::anyhow!("failed to read checkpoint blob: {e}"))?;
        let Some(bytes) = blob else { continue };
        let cp: Checkpoint = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse checkpoint at {}", commit.oid))?;

        ordinal += 1;
        let visit = cp.node_visits.get(&cp.current_node).copied().unwrap_or(1);

        timeline.push(TimelineEntry {
            ordinal,
            node_name: cp.current_node.clone(),
            visit,
            metadata_commit_oid: commit.oid,
            run_commit_sha: cp.git_commit_sha.clone(),
        });
    }

    // Backfill missing git_commit_sha from run branch commit messages
    backfill_run_shas(store, run_id, &mut timeline);

    Ok(timeline)
}

/// Walk the run branch and match commits by message pattern to backfill missing SHAs.
fn backfill_run_shas(store: &Store, run_id: &str, timeline: &mut [TimelineEntry]) {
    let needs_backfill = timeline.iter().any(|e| e.run_commit_sha.is_none());
    if !needs_backfill {
        return;
    }

    let run_branch = format!("{}{run_id}", crate::git::RUN_BRANCH_PREFIX);
    let sig = match Signature::now("Fabro", "noreply@fabro.sh") {
        Ok(s) => s,
        Err(_) => return,
    };
    let bs = BranchStore::new(store, &run_branch, &sig);
    let run_commits = match bs.log(10_000) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Build a map from node_name to Vec<commit SHA> (newest-first from log)
    let prefix = format!("fabro({run_id}): ");
    let mut node_commits: HashMap<String, Vec<String>> = HashMap::new();
    for commit in &run_commits {
        if let Some(rest) = commit.message.strip_prefix(&prefix) {
            // Message format: "fabro({run_id}): {node_name} ({status})"
            if let Some(node_name) = rest.split_whitespace().next() {
                node_commits
                    .entry(node_name.to_string())
                    .or_default()
                    .push(commit.oid.to_string());
            }
        }
    }

    // Assign SHAs to timeline entries that are missing them.
    // For each node, pop from the end (oldest) to match visit order.
    for (_, shas) in node_commits.iter_mut() {
        shas.reverse(); // oldest-first
    }
    let mut node_indices: HashMap<String, usize> = HashMap::new();

    for entry in timeline.iter_mut() {
        if entry.run_commit_sha.is_some() {
            continue;
        }
        if let Some(shas) = node_commits.get(&entry.node_name) {
            let idx = node_indices.entry(entry.node_name.clone()).or_insert(0);
            if *idx < shas.len() {
                entry.run_commit_sha = Some(shas[*idx].clone());
                *idx += 1;
            }
        }
    }
}

/// Map interior parallel nodes to their fan-out parallel node ID.
pub fn detect_parallel_interior(graph: &Graph) -> HashMap<String, String> {
    let mut interior_map = HashMap::new();

    for node in graph.nodes.values() {
        if node.handler_type() != Some("parallel") {
            continue;
        }
        let parallel_id = &node.id;
        // BFS from parallel node to find interior nodes until we hit the fan_in
        let mut queue: Vec<String> = graph
            .outgoing_edges(parallel_id)
            .iter()
            .map(|e| e.to.clone())
            .collect();
        let mut visited = std::collections::HashSet::new();

        while let Some(current) = queue.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(n) = graph.nodes.get(&current) {
                if n.handler_type() == Some("parallel.fan_in") {
                    continue; // don't traverse past fan_in
                }
            }
            interior_map.insert(current.clone(), parallel_id.clone());
            for edge in graph.outgoing_edges(&current) {
                queue.push(edge.to.clone());
            }
        }
    }

    interior_map
}

/// Resolve a target to a timeline entry, with parallel snap-back.
pub fn resolve_target<'a>(
    timeline: &'a [TimelineEntry],
    target: &RewindTarget,
    parallel_map: &HashMap<String, String>,
) -> Result<&'a TimelineEntry> {
    match target {
        RewindTarget::Ordinal(n) => timeline
            .iter()
            .find(|e| e.ordinal == *n)
            .ok_or_else(|| anyhow::anyhow!("ordinal @{n} out of range (max @{})", timeline.len())),

        RewindTarget::LatestVisit(name) => {
            let effective_name = parallel_map.get(name).unwrap_or(name);
            timeline
                .iter()
                .rev()
                .find(|e| e.node_name == *effective_name)
                .ok_or_else(|| {
                    if effective_name != name {
                        anyhow::anyhow!(
                            "node '{name}' is inside parallel '{effective_name}'; \
                             no checkpoint found for '{effective_name}'"
                        )
                    } else {
                        anyhow::anyhow!("no checkpoint found for node '{name}'")
                    }
                })
        }

        RewindTarget::SpecificVisit(name, visit) => {
            let effective_name = parallel_map.get(name).unwrap_or(name);
            timeline
                .iter()
                .find(|e| e.node_name == *effective_name && e.visit == *visit)
                .ok_or_else(|| {
                    if effective_name != name {
                        anyhow::anyhow!(
                            "node '{name}' is inside parallel '{effective_name}'; \
                             no visit {visit} found for '{effective_name}'"
                        )
                    } else {
                        anyhow::anyhow!("no visit {visit} found for node '{name}'")
                    }
                })
        }
    }
}

/// Move both refs backward to the target checkpoint.
pub fn execute_rewind(
    store: &Store,
    run_id: &str,
    entry: &TimelineEntry,
    push: bool,
) -> Result<()> {
    // Move metadata branch ref
    let meta_branch = MetadataStore::branch_name(run_id);
    store
        .update_ref(&meta_branch, entry.metadata_commit_oid)
        .map_err(|e| anyhow::anyhow!("failed to update metadata ref: {e}"))?;
    eprintln!(
        "Rewound metadata branch to @{} ({})",
        entry.ordinal, entry.node_name
    );

    // Move run branch ref
    let run_branch = format!("{}{run_id}", crate::git::RUN_BRANCH_PREFIX);
    match &entry.run_commit_sha {
        Some(sha) => {
            let oid =
                Oid::from_str(sha).with_context(|| format!("invalid run commit SHA: {sha}"))?;
            store
                .update_ref(&run_branch, oid)
                .map_err(|e| anyhow::anyhow!("failed to update run branch ref: {e}"))?;
            eprintln!(
                "Rewound run branch {}{run_id} to {}",
                crate::git::RUN_BRANCH_PREFIX,
                &sha[..8]
            );
        }
        None => {
            eprintln!(
                "Warning: checkpoint @{} has no git_commit_sha; run branch not moved",
                entry.ordinal
            );
        }
    }

    // Optionally push to remote
    if push {
        let repo_path = store
            .repo()
            .workdir()
            .or_else(|| store.repo().path().parent())
            .unwrap_or(store.repo().path());

        // Check if run branch has a remote tracking ref
        let remote_ref = format!("refs/remotes/origin/{run_branch}");
        let has_remote_tracking = store.repo().find_reference(&remote_ref).is_ok();

        if has_remote_tracking {
            eprintln!("Force-pushing rewound branches to origin...");

            // Force-push run branch
            if entry.run_commit_sha.is_some() {
                let refspec = format!("+refs/heads/{run_branch}:refs/heads/{run_branch}");
                crate::git::push_branch(repo_path, "origin", &refspec)
                    .map_err(|e| anyhow::anyhow!("failed to push run branch: {e}"))?;
            }

            // Force-push metadata branch
            let meta_refspec = format!("+refs/heads/{meta_branch}:refs/heads/{meta_branch}");
            crate::git::push_branch(repo_path, "origin", &meta_refspec)
                .map_err(|e| anyhow::anyhow!("failed to push metadata branch: {e}"))?;

            eprintln!("Remote refs updated.");
        }
    }

    Ok(())
}

/// Find a run ID by exact match or unambiguous prefix.
pub fn find_run_id_by_prefix(repo: &Repository, prefix: &str) -> Result<String> {
    let refs = repo.references()?;
    let pattern = "refs/heads/fabro/meta/";
    let mut matches = Vec::new();

    for reference in refs.flatten() {
        let name = match reference.name() {
            Some(n) => n,
            None => continue,
        };
        if let Some(run_id) = name.strip_prefix(pattern) {
            if run_id == prefix {
                return Ok(run_id.to_string());
            }
            if run_id.starts_with(prefix) {
                matches.push(run_id.to_string());
            }
        }
    }

    match matches.len() {
        0 => bail!("no run found matching '{prefix}'"),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let mut msg = format!("ambiguous run ID prefix '{prefix}', matches:\n");
            for m in &matches {
                msg.push_str(&format!("  {m}\n"));
            }
            bail!("{msg}")
        }
    }
}

/// Load the graph from the metadata branch and build the parallel interior map.
///
/// Tries `run.json` (RunRecord with embedded Graph) first, then falls back to
/// parsing `graph.fabro` DOT source for backward compatibility.
pub fn load_parallel_map(store: &Store, run_id: &str) -> HashMap<String, String> {
    let branch = MetadataStore::branch_name(run_id);
    let sig = match Signature::now("Fabro", "noreply@fabro.sh") {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let bs = BranchStore::new(store, &branch, &sig);

    // Try run.json first (RunRecord with embedded Graph)
    if let Ok(Some(run_bytes)) = bs.read_entry("run.json") {
        if let Ok(record) = serde_json::from_slice::<crate::run_record::RunRecord>(&run_bytes) {
            return detect_parallel_interior(&record.graph);
        }
    }

    // Fallback: parse graph.fabro DOT
    let graph_bytes = match bs.read_entry("graph.fabro") {
        Ok(Some(bytes)) => bytes,
        _ => return HashMap::new(),
    };
    let dot_source = String::from_utf8_lossy(&graph_bytes);
    let graph = match fabro_graphviz::parser::parse(&dot_source) {
        Ok(g) => g,
        Err(_) => return HashMap::new(),
    };
    detect_parallel_interior(&graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_target tests --

    #[test]
    fn parse_target_ordinal() {
        assert_eq!(parse_target("@4").unwrap(), RewindTarget::Ordinal(4));
    }

    #[test]
    fn parse_target_ordinal_one() {
        assert_eq!(parse_target("@1").unwrap(), RewindTarget::Ordinal(1));
    }

    #[test]
    fn parse_target_ordinal_zero_errors() {
        assert!(parse_target("@0").is_err());
    }

    #[test]
    fn parse_target_latest_visit() {
        assert_eq!(
            parse_target("step2").unwrap(),
            RewindTarget::LatestVisit("step2".to_string())
        );
    }

    #[test]
    fn parse_target_specific_visit() {
        assert_eq!(
            parse_target("step3@2").unwrap(),
            RewindTarget::SpecificVisit("step3".to_string(), 2)
        );
    }

    #[test]
    fn parse_target_specific_visit_zero_errors() {
        assert!(parse_target("step3@0").is_err());
    }

    // -- build_timeline tests --

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

    #[test]
    fn build_timeline_simple() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("test-run-1");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        // init commit (should be skipped)
        bs.write_entry("run.json", b"{}", "init run").unwrap();

        // 3 checkpoint commits
        let cp1 = make_checkpoint_json("start", 1, Some("aaa"));
        bs.write_entry("checkpoint.json", &cp1, "checkpoint")
            .unwrap();

        let cp2 = make_checkpoint_json("build", 1, Some("bbb"));
        bs.write_entry("checkpoint.json", &cp2, "checkpoint")
            .unwrap();

        let cp3 = make_checkpoint_json("test", 1, Some("ccc"));
        bs.write_entry("checkpoint.json", &cp3, "checkpoint")
            .unwrap();

        let timeline = build_timeline(&store, "test-run-1").unwrap();
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0].ordinal, 1);
        assert_eq!(timeline[0].node_name, "start");
        assert_eq!(timeline[0].visit, 1);
        assert_eq!(timeline[1].ordinal, 2);
        assert_eq!(timeline[1].node_name, "build");
        assert_eq!(timeline[2].ordinal, 3);
        assert_eq!(timeline[2].node_name, "test");
    }

    #[test]
    fn build_timeline_skips_non_checkpoint_commits() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("test-run-2");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("run.json", b"{}", "init run").unwrap();

        let cp1 = make_checkpoint_json("start", 1, None);
        bs.write_entry("checkpoint.json", &cp1, "checkpoint")
            .unwrap();

        // finalize commit — should be skipped
        bs.write_entry("retro.json", b"{}", "finalize").unwrap();

        let timeline = build_timeline(&store, "test-run-2").unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].node_name, "start");
    }

    // -- resolve_target tests --

    fn make_timeline() -> Vec<TimelineEntry> {
        vec![
            TimelineEntry {
                ordinal: 1,
                node_name: "start".to_string(),
                visit: 1,
                metadata_commit_oid: Oid::zero(),
                run_commit_sha: Some("aaa".to_string()),
            },
            TimelineEntry {
                ordinal: 2,
                node_name: "build".to_string(),
                visit: 1,
                metadata_commit_oid: Oid::zero(),
                run_commit_sha: Some("bbb".to_string()),
            },
            TimelineEntry {
                ordinal: 3,
                node_name: "build".to_string(),
                visit: 2,
                metadata_commit_oid: Oid::zero(),
                run_commit_sha: Some("ccc".to_string()),
            },
        ]
    }

    #[test]
    fn resolve_ordinal() {
        let timeline = make_timeline();
        let entry = resolve_target(&timeline, &RewindTarget::Ordinal(2), &HashMap::new()).unwrap();
        assert_eq!(entry.ordinal, 2);
        assert_eq!(entry.node_name, "build");
    }

    #[test]
    fn resolve_latest_visit() {
        let timeline = make_timeline();
        let entry = resolve_target(
            &timeline,
            &RewindTarget::LatestVisit("build".to_string()),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(entry.ordinal, 3);
        assert_eq!(entry.visit, 2);
    }

    #[test]
    fn resolve_specific_visit() {
        let timeline = make_timeline();
        let entry = resolve_target(
            &timeline,
            &RewindTarget::SpecificVisit("build".to_string(), 1),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(entry.ordinal, 2);
        assert_eq!(entry.visit, 1);
    }

    #[test]
    fn resolve_ordinal_out_of_range() {
        let timeline = make_timeline();
        let result = resolve_target(&timeline, &RewindTarget::Ordinal(99), &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of range"));
    }

    #[test]
    fn resolve_unknown_node() {
        let timeline = make_timeline();
        let result = resolve_target(
            &timeline,
            &RewindTarget::LatestVisit("nonexistent".to_string()),
            &HashMap::new(),
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no checkpoint found"));
    }

    // -- detect_parallel_interior tests --

    #[test]
    fn parallel_interior_detection() {
        let mut graph = Graph::new("test");
        let mut parallel_node = fabro_graphviz::graph::Node::new("parallel1");
        parallel_node.attrs.insert(
            "shape".to_string(),
            fabro_graphviz::graph::AttrValue::String("component".to_string()),
        );
        graph.nodes.insert("parallel1".to_string(), parallel_node);

        let mut fan_in = fabro_graphviz::graph::Node::new("fan_in1");
        fan_in.attrs.insert(
            "shape".to_string(),
            fabro_graphviz::graph::AttrValue::String("tripleoctagon".to_string()),
        );
        graph.nodes.insert("fan_in1".to_string(), fan_in);

        let mut a = fabro_graphviz::graph::Node::new("a");
        a.attrs.insert(
            "shape".to_string(),
            fabro_graphviz::graph::AttrValue::String("box".to_string()),
        );
        graph.nodes.insert("a".to_string(), a);

        let mut b = fabro_graphviz::graph::Node::new("b");
        b.attrs.insert(
            "shape".to_string(),
            fabro_graphviz::graph::AttrValue::String("box".to_string()),
        );
        graph.nodes.insert("b".to_string(), b);

        graph.edges.push(fabro_graphviz::graph::Edge {
            from: "parallel1".to_string(),
            to: "a".to_string(),
            attrs: HashMap::new(),
        });
        graph.edges.push(fabro_graphviz::graph::Edge {
            from: "parallel1".to_string(),
            to: "b".to_string(),
            attrs: HashMap::new(),
        });
        graph.edges.push(fabro_graphviz::graph::Edge {
            from: "a".to_string(),
            to: "fan_in1".to_string(),
            attrs: HashMap::new(),
        });
        graph.edges.push(fabro_graphviz::graph::Edge {
            from: "b".to_string(),
            to: "fan_in1".to_string(),
            attrs: HashMap::new(),
        });

        let map = detect_parallel_interior(&graph);
        assert_eq!(map.get("a"), Some(&"parallel1".to_string()));
        assert_eq!(map.get("b"), Some(&"parallel1".to_string()));
        assert!(!map.contains_key("parallel1"));
        assert!(!map.contains_key("fan_in1"));
    }

    #[test]
    fn parallel_snap_back() {
        let timeline = vec![
            TimelineEntry {
                ordinal: 1,
                node_name: "parallel1".to_string(),
                visit: 1,
                metadata_commit_oid: Oid::zero(),
                run_commit_sha: Some("aaa".to_string()),
            },
            TimelineEntry {
                ordinal: 2,
                node_name: "a".to_string(),
                visit: 1,
                metadata_commit_oid: Oid::zero(),
                run_commit_sha: Some("bbb".to_string()),
            },
        ];

        let mut parallel_map = HashMap::new();
        parallel_map.insert("a".to_string(), "parallel1".to_string());

        // Targeting "a" should snap back to "parallel1"
        let entry = resolve_target(
            &timeline,
            &RewindTarget::LatestVisit("a".to_string()),
            &parallel_map,
        )
        .unwrap();
        assert_eq!(entry.node_name, "parallel1");
        assert_eq!(entry.ordinal, 1);
    }

    // -- execute_rewind tests --

    #[test]
    fn execute_rewind_moves_metadata_ref() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("run-1");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("run.json", b"{}", "init run").unwrap();

        let cp1 = make_checkpoint_json("start", 1, None);
        let oid1 = bs
            .write_entry("checkpoint.json", &cp1, "checkpoint")
            .unwrap();

        let cp2 = make_checkpoint_json("build", 1, None);
        bs.write_entry("checkpoint.json", &cp2, "checkpoint")
            .unwrap();

        let cp3 = make_checkpoint_json("test", 1, None);
        bs.write_entry("checkpoint.json", &cp3, "checkpoint")
            .unwrap();

        let timeline = build_timeline(&store, "run-1").unwrap();
        let entry = &timeline[0]; // @1 = start

        execute_rewind(&store, "run-1", entry, false).unwrap();

        // Verify metadata ref points to the @1 commit
        let resolved = store.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(resolved, oid1);
    }

    #[test]
    fn execute_rewind_moves_run_branch_ref() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();

        // Create a run branch with some commits
        let run_branch = "fabro/run/run-2";
        let empty_tree = store.write_empty_tree().unwrap();
        let run_c1 = store
            .write_commit(empty_tree, &[], "run commit 1", &sig)
            .unwrap();
        store.update_ref(run_branch, run_c1).unwrap();
        let run_c2 = store
            .write_commit(empty_tree, &[run_c1], "run commit 2", &sig)
            .unwrap();
        store.update_ref(run_branch, run_c2).unwrap();

        // Create metadata branch with checkpoints pointing to run commits
        let meta_branch = MetadataStore::branch_name("run-2");
        let meta_bs = BranchStore::new(&store, &meta_branch, &sig);
        meta_bs.ensure_branch().unwrap();
        meta_bs.write_entry("run.json", b"{}", "init run").unwrap();

        let cp1 = make_checkpoint_json("start", 1, Some(&run_c1.to_string()));
        meta_bs
            .write_entry("checkpoint.json", &cp1, "checkpoint")
            .unwrap();

        let cp2 = make_checkpoint_json("build", 1, Some(&run_c2.to_string()));
        meta_bs
            .write_entry("checkpoint.json", &cp2, "checkpoint")
            .unwrap();

        let timeline = build_timeline(&store, "run-2").unwrap();
        let entry = &timeline[0]; // @1

        execute_rewind(&store, "run-2", entry, false).unwrap();

        // Verify run branch ref moved to run_c1
        let resolved = store.resolve_ref(run_branch).unwrap().unwrap();
        assert_eq!(resolved, run_c1);
    }

    #[test]
    fn execute_rewind_warns_on_missing_run_sha() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("run-3");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("run.json", b"{}", "init run").unwrap();

        let cp1 = make_checkpoint_json("start", 1, None);
        let oid1 = bs
            .write_entry("checkpoint.json", &cp1, "checkpoint")
            .unwrap();

        let timeline = build_timeline(&store, "run-3").unwrap();

        // Should not panic even though run_commit_sha is None
        execute_rewind(&store, "run-3", &timeline[0], false).unwrap();

        // Metadata ref should still be moved
        let resolved = store.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(resolved, oid1);
    }

    // -- find_run_id_by_prefix tests --

    #[test]
    fn find_run_id_exact_match() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("abc-123");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        let result = find_run_id_by_prefix(store.repo(), "abc-123").unwrap();
        assert_eq!(result, "abc-123");
    }

    #[test]
    fn find_run_id_prefix_match() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("abc-123-long-id");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        let result = find_run_id_by_prefix(store.repo(), "abc-123").unwrap();
        assert_eq!(result, "abc-123-long-id");
    }

    #[test]
    fn find_run_id_ambiguous() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();

        let b1 = MetadataStore::branch_name("abc-111");
        BranchStore::new(&store, &b1, &sig).ensure_branch().unwrap();

        let b2 = MetadataStore::branch_name("abc-222");
        BranchStore::new(&store, &b2, &sig).ensure_branch().unwrap();

        let result = find_run_id_by_prefix(store.repo(), "abc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ambiguous"));
    }

    #[test]
    fn find_run_id_not_found() {
        let (_dir, store) = temp_repo();
        let result = find_run_id_by_prefix(store.repo(), "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no run found"));
    }

    #[test]
    fn rewind_push_refspec_uses_same_name_on_both_sides() {
        // The meta branch name should work directly as a refspec
        // without needing strip_prefix translation
        let meta_branch = MetadataStore::branch_name("run-1");
        let refspec = format!("+refs/heads/{meta_branch}:refs/heads/{meta_branch}");
        assert_eq!(
            refspec,
            "+refs/heads/fabro/meta/run-1:refs/heads/fabro/meta/run-1"
        );
    }
}
