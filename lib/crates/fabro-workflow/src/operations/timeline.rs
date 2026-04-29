use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use fabro_graphviz::graph::Graph;
use fabro_graphviz::parser;
use fabro_store::{Database, RunProjection};
use fabro_types::RunId;

use crate::error::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkTarget {
    Ordinal(usize),
    LatestVisit(String),
    SpecificVisit(String, usize),
}

impl FromStr for ForkTarget {
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
    pub ordinal:        usize,
    pub node_name:      String,
    pub visit:          usize,
    pub checkpoint_seq: u32,
    pub run_commit_sha: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunTimeline {
    pub entries:      Vec<TimelineEntry>,
    pub parallel_map: HashMap<String, String>,
}

impl RunTimeline {
    pub fn resolve(&self, target: &ForkTarget) -> Result<&TimelineEntry> {
        match target {
            ForkTarget::Ordinal(n) => {
                self.entries
                    .iter()
                    .find(|e| e.ordinal == *n)
                    .ok_or_else(|| {
                        anyhow::anyhow!("ordinal @{n} out of range (max @{})", self.entries.len())
                    })
            }
            ForkTarget::LatestVisit(name) => {
                let effective_name = self.parallel_map.get(name).unwrap_or(name);
                self.entries
                    .iter()
                    .rev()
                    .find(|e| e.node_name == *effective_name)
                    .ok_or_else(|| {
                        if effective_name == name {
                            anyhow::anyhow!("no checkpoint found for node '{name}'")
                        } else {
                            anyhow::anyhow!(
                                "node '{name}' is inside parallel '{effective_name}'; \
                                 no checkpoint found for '{effective_name}'"
                            )
                        }
                    })
            }
            ForkTarget::SpecificVisit(name, visit) => {
                let effective_name = self.parallel_map.get(name).unwrap_or(name);
                self.entries
                    .iter()
                    .find(|e| e.node_name == *effective_name && e.visit == *visit)
                    .ok_or_else(|| {
                        if effective_name == name {
                            anyhow::anyhow!("no visit {visit} found for node '{name}'")
                        } else {
                            anyhow::anyhow!(
                                "node '{name}' is inside parallel '{effective_name}'; \
                                 no visit {visit} found for '{effective_name}'"
                            )
                        }
                    })
            }
        }
    }
}

pub fn build_timeline(state: &RunProjection) -> Result<RunTimeline> {
    let mut entries = Vec::new();
    for (seq, checkpoint) in &state.checkpoints {
        let ordinal = entries.len() + 1;
        let visit = checkpoint
            .node_visits
            .get(&checkpoint.current_node)
            .copied()
            .unwrap_or(1);
        entries.push(TimelineEntry {
            ordinal,
            node_name: checkpoint.current_node.clone(),
            visit,
            checkpoint_seq: *seq,
            run_commit_sha: checkpoint.git_commit_sha.clone(),
        });
    }

    Ok(RunTimeline {
        entries,
        parallel_map: load_parallel_map(state),
    })
}

pub async fn timeline(store: &Database, run_id: &RunId) -> Result<Vec<TimelineEntry>, Error> {
    let run = store
        .open_run(run_id)
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    let state = run
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))?;
    build_timeline(&state)
        .map(|timeline| timeline.entries)
        .map_err(|err| Error::engine(err.to_string()))
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

fn load_parallel_map(state: &RunProjection) -> HashMap<String, String> {
    if let Some(spec) = state.spec.as_ref() {
        return detect_parallel_interior(&spec.graph);
    }

    let Some(dot_source) = state.graph_source.as_ref() else {
        return HashMap::new();
    };
    let Ok(graph) = parser::parse(dot_source) else {
        return HashMap::new();
    };
    detect_parallel_interior(&graph)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use fabro_types::Checkpoint;

    use super::*;

    fn checkpoint(
        seq: u32,
        current_node: &str,
        visit: usize,
        git_commit_sha: Option<&str>,
    ) -> (u32, Checkpoint) {
        let mut node_visits = HashMap::new();
        node_visits.insert(current_node.to_string(), visit);
        let checkpoint = Checkpoint {
            timestamp: Utc::now(),
            current_node: current_node.to_string(),
            completed_nodes: Vec::new(),
            node_retries: HashMap::new(),
            context_values: HashMap::new(),
            node_outcomes: HashMap::new(),
            next_node_id: None,
            git_commit_sha: git_commit_sha.map(ToOwned::to_owned),
            loop_failure_signatures: HashMap::new(),
            restart_failure_signatures: HashMap::new(),
            node_visits,
        };
        (seq, checkpoint)
    }

    #[test]
    fn parse_target_ordinal() {
        assert_eq!("@4".parse::<ForkTarget>().unwrap(), ForkTarget::Ordinal(4));
    }

    #[test]
    fn parse_target_latest_visit() {
        assert_eq!(
            "step2".parse::<ForkTarget>().unwrap(),
            ForkTarget::LatestVisit("step2".to_string())
        );
    }

    #[test]
    fn build_timeline_simple() {
        let mut state = RunProjection::default();
        state.checkpoints = vec![
            checkpoint(7, "start", 1, Some("aaa")),
            checkpoint(9, "build", 1, Some("bbb")),
        ];

        let timeline = build_timeline(&state).unwrap();
        assert_eq!(timeline.entries.len(), 2);
        assert_eq!(timeline.entries[0].node_name, "start");
        assert_eq!(timeline.entries[0].checkpoint_seq, 7);
        assert_eq!(timeline.entries[1].node_name, "build");
    }

    #[test]
    fn resolve_latest_visit() {
        let timeline = RunTimeline {
            entries:      vec![
                TimelineEntry {
                    ordinal:        1,
                    node_name:      "start".to_string(),
                    visit:          1,
                    checkpoint_seq: 7,
                    run_commit_sha: Some("aaa".to_string()),
                },
                TimelineEntry {
                    ordinal:        2,
                    node_name:      "build".to_string(),
                    visit:          1,
                    checkpoint_seq: 9,
                    run_commit_sha: Some("bbb".to_string()),
                },
                TimelineEntry {
                    ordinal:        3,
                    node_name:      "build".to_string(),
                    visit:          2,
                    checkpoint_seq: 11,
                    run_commit_sha: Some("ccc".to_string()),
                },
            ],
            parallel_map: HashMap::new(),
        };

        let entry = timeline
            .resolve(&ForkTarget::LatestVisit("build".to_string()))
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
            from:  "parallel1".to_string(),
            to:    "a".to_string(),
            attrs: HashMap::new(),
        });
        graph.edges.push(fabro_graphviz::graph::Edge {
            from:  "a".to_string(),
            to:    "fan_in1".to_string(),
            attrs: HashMap::new(),
        });

        let map = detect_parallel_interior(&graph);
        assert_eq!(map.get("a"), Some(&"parallel1".to_string()));
        assert!(!map.contains_key("parallel1"));
    }
}
