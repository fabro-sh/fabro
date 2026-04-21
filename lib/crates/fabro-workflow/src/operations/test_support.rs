use std::collections::HashMap;

use fabro_checkpoint::git::Store;
use git2::{Repository, Signature};

pub(super) fn temp_repo() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    (dir, Store::new(repo))
}

pub(super) fn test_sig() -> Signature<'static> {
    Signature::now("Test", "test@example.com").unwrap()
}

pub(super) fn make_checkpoint_bytes(
    current_node: &str,
    visit: usize,
    git_sha: Option<&str>,
) -> Vec<u8> {
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
