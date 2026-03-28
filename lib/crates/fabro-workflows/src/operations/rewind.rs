use std::collections::HashMap;
use std::fmt::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use fabro_git_storage::branchstore::{BranchStore, CommitInfo};
use fabro_git_storage::gitobj::Store;
use git2::{Oid, Repository, Signature};

use crate::git::{MetadataStore, RUN_BRANCH_PREFIX, push_run_branches};
use crate::records::{Checkpoint, RunRecord};
use fabro_graphviz::graph::Graph;
use fabro_graphviz::parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindTarget {
    Ordinal(usize),
    LatestVisit(String),
    SpecificVisit(String, usize),
}

impl FromStr for RewindTarget {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix('@') {
            let n: usize = rest
                .parse()
                .with_context(|| format!("invalid ordinal: @{rest}"))?;
            if n == 0 {
                bail!("ordinal must be >= 1");
            }
            return Ok(Self::Ordinal(n));
        }
        if let Some(at_pos) = s.rfind('@') {
            let name = &s[..at_pos];
            let visit_str = &s[at_pos + 1..];
            if !name.is_empty() && !visit_str.is_empty() {
                if let Ok(visit) = visit_str.parse::<usize>() {
                    if visit == 0 {
                        bail!("visit number must be >= 1");
                    }
                    return Ok(Self::SpecificVisit(name.to_string(), visit));
                }
            }
        }
        Ok(Self::LatestVisit(s.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub ordinal: usize,
    pub node_name: String,
    pub visit: usize,
    pub metadata_commit_oid: Oid,
    pub run_commit_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunTimeline {
    pub entries: Vec<TimelineEntry>,
    pub parallel_map: HashMap<String, String>,
}

impl RunTimeline {
    pub fn resolve(&self, target: &RewindTarget) -> Result<&TimelineEntry> {
        match target {
            RewindTarget::Ordinal(n) => {
                self.entries
                    .iter()
                    .find(|e| e.ordinal == *n)
                    .ok_or_else(|| {
                        anyhow::anyhow!("ordinal @{n} out of range (max @{})", self.entries.len())
                    })
            }
            RewindTarget::LatestVisit(name) => {
                let effective_name = self.parallel_map.get(name).unwrap_or(name);
                self.entries
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
                let effective_name = self.parallel_map.get(name).unwrap_or(name);
                self.entries
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
}

#[derive(Debug, Clone)]
pub struct RewindInput {
    pub run_id: String,
    pub target: RewindTarget,
    pub push: bool,
}

pub fn build_timeline(store: &Store, run_id: &str) -> Result<RunTimeline> {
    let branch = MetadataStore::branch_name(run_id);
    let sig = Signature::now("Fabro", "noreply@fabro.sh")?;
    let bs = BranchStore::new(store, &branch, &sig);

    let commits = bs
        .log(10_000)
        .map_err(|e| anyhow::anyhow!("failed to read metadata branch log: {e}"))?;
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

    backfill_run_shas(store, run_id, &mut timeline);
    Ok(RunTimeline {
        entries: timeline,
        parallel_map: load_parallel_map(store, run_id),
    })
}

fn backfill_run_shas(store: &Store, run_id: &str, timeline: &mut [TimelineEntry]) {
    if !timeline.iter().any(|e| e.run_commit_sha.is_none()) {
        return;
    }

    let run_branch = format!("{RUN_BRANCH_PREFIX}{run_id}");
    let Ok(sig) = Signature::now("Fabro", "noreply@fabro.sh") else {
        return;
    };
    let bs = BranchStore::new(store, &run_branch, &sig);
    let Ok(run_commits) = bs.log(10_000) else {
        return;
    };

    let prefix = format!("fabro({run_id}): ");
    let mut node_commits: HashMap<String, Vec<String>> = HashMap::new();
    for commit in &run_commits {
        if let Some(rest) = commit.message.strip_prefix(&prefix) {
            if let Some(node_name) = rest.split_whitespace().next() {
                node_commits
                    .entry(node_name.to_string())
                    .or_default()
                    .push(commit.oid.to_string());
            }
        }
    }

    for shas in node_commits.values_mut() {
        shas.reverse();
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

fn detect_parallel_interior(graph: &Graph) -> HashMap<String, String> {
    let mut interior_map = HashMap::new();

    for node in graph.nodes.values() {
        if node.handler_type() != Some("parallel") {
            continue;
        }
        let parallel_id = &node.id;
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
                    continue;
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

pub fn rewind(store: &Store, input: &RewindInput) -> Result<()> {
    let timeline = build_timeline(store, &input.run_id)?;
    let entry = timeline.resolve(&input.target)?;
    rewind_to_entry(store, &input.run_id, entry, input.push)
}

#[allow(clippy::print_stderr)]
fn rewind_to_entry(store: &Store, run_id: &str, entry: &TimelineEntry, push: bool) -> Result<()> {
    let meta_branch = MetadataStore::branch_name(run_id);
    store
        .update_ref(&meta_branch, entry.metadata_commit_oid)
        .map_err(|e| anyhow::anyhow!("failed to update metadata ref: {e}"))?;
    eprintln!(
        "Rewound metadata branch to @{} ({})",
        entry.ordinal, entry.node_name
    );

    let run_branch = format!("{RUN_BRANCH_PREFIX}{run_id}");
    match &entry.run_commit_sha {
        Some(sha) => {
            let oid =
                Oid::from_str(sha).with_context(|| format!("invalid run commit SHA: {sha}"))?;
            store
                .update_ref(&run_branch, oid)
                .map_err(|e| anyhow::anyhow!("failed to update run branch ref: {e}"))?;
            eprintln!(
                "Rewound run branch {}{run_id} to {}",
                RUN_BRANCH_PREFIX,
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

    if push {
        let run_refspec = entry
            .run_commit_sha
            .as_ref()
            .map(|_| format!("+refs/heads/{run_branch}:refs/heads/{run_branch}"));
        let meta_refspec = format!("+refs/heads/{meta_branch}:refs/heads/{meta_branch}");
        push_run_branches(
            store,
            &run_branch,
            run_refspec.as_deref(),
            &meta_refspec,
            "rewound",
        )?;
    }

    Ok(())
}

pub fn find_run_id_by_prefix(repo: &Repository, prefix: &str) -> Result<String> {
    let refs = repo.references()?;
    let pattern = "refs/heads/fabro/meta/";
    let mut matches = Vec::new();

    for reference in refs.flatten() {
        let Some(name) = reference.name() else {
            continue;
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
                let _ = writeln!(msg, "  {m}");
            }
            bail!("{msg}")
        }
    }
}

fn load_parallel_map(store: &Store, run_id: &str) -> HashMap<String, String> {
    let branch = MetadataStore::branch_name(run_id);
    let Ok(sig) = Signature::now("Fabro", "noreply@fabro.sh") else {
        return HashMap::new();
    };
    let bs = BranchStore::new(store, &branch, &sig);

    if let Ok(Some(run_bytes)) = bs.read_entry("run.json") {
        if let Ok(record) = serde_json::from_slice::<RunRecord>(&run_bytes) {
            return detect_parallel_interior(&record.graph);
        }
    }

    let graph_bytes = match bs.read_entry("workflow.fabro") {
        Ok(Some(bytes)) => bytes,
        _ => match bs.read_entry("graph.fabro") {
            Ok(Some(bytes)) => bytes,
            _ => return HashMap::new(),
        },
    };
    let dot_source = String::from_utf8_lossy(&graph_bytes);
    let Ok(graph) = parser::parse(&dot_source) else {
        return HashMap::new();
    };
    detect_parallel_interior(&graph)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn parse_target_ordinal() {
        assert_eq!(
            "@4".parse::<RewindTarget>().unwrap(),
            RewindTarget::Ordinal(4)
        );
    }

    #[test]
    fn parse_target_latest_visit() {
        assert_eq!(
            "step2".parse::<RewindTarget>().unwrap(),
            RewindTarget::LatestVisit("step2".to_string())
        );
    }

    #[test]
    fn build_timeline_simple() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let branch = MetadataStore::branch_name("test-run-1");
        let bs = BranchStore::new(&store, &branch, &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("run.json", b"{}", "init run").unwrap();
        let cp1 = make_checkpoint_json("start", 1, Some("aaa"));
        bs.write_entry("checkpoint.json", &cp1, "checkpoint")
            .unwrap();
        let cp2 = make_checkpoint_json("build", 1, Some("bbb"));
        bs.write_entry("checkpoint.json", &cp2, "checkpoint")
            .unwrap();

        let timeline = build_timeline(&store, "test-run-1").unwrap();
        assert_eq!(timeline.entries.len(), 2);
        assert_eq!(timeline.entries[0].node_name, "start");
        assert_eq!(timeline.entries[1].node_name, "build");
    }

    #[test]
    fn resolve_latest_visit() {
        let timeline = RunTimeline {
            entries: vec![
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
            ],
            parallel_map: HashMap::new(),
        };

        let entry = timeline
            .resolve(&RewindTarget::LatestVisit("build".to_string()))
            .unwrap();
        assert_eq!(entry.ordinal, 3);
    }

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

        graph.edges.push(fabro_graphviz::graph::Edge {
            from: "parallel1".to_string(),
            to: "a".to_string(),
            attrs: HashMap::new(),
        });
        graph.edges.push(fabro_graphviz::graph::Edge {
            from: "a".to_string(),
            to: "fan_in1".to_string(),
            attrs: HashMap::new(),
        });

        let map = detect_parallel_interior(&graph);
        assert_eq!(map.get("a"), Some(&"parallel1".to_string()));
        assert!(!map.contains_key("parallel1"));
    }

    #[test]
    fn rewind_moves_metadata_ref() {
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

        rewind(
            &store,
            &RewindInput {
                run_id: "run-1".to_string(),
                target: RewindTarget::Ordinal(1),
                push: false,
            },
        )
        .unwrap();

        let resolved = store.resolve_ref(&branch).unwrap().unwrap();
        assert_eq!(resolved, oid1);
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
}
