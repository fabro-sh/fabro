use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::context::Context;
use crate::outcome::{Outcome, OutcomeExt};
use crate::run_settings::RunSettings;

/// Write start.json at the start of a workflow run. Returns the StartRecord.
pub(crate) fn write_start_record(
    run_dir: &Path,
    settings: &RunSettings,
) -> crate::records::StartRecord {
    let git_state = settings.git.as_ref();
    let record = crate::records::StartRecord {
        run_id: settings.run_id.clone(),
        start_time: Utc::now(),
        run_branch: git_state.and_then(|g| g.run_branch.clone()),
        base_sha: git_state.and_then(|g| g.base_sha.clone()),
    };
    let _ = std::fs::create_dir_all(run_dir);
    let _ = record.save(run_dir);
    record
}

/// Return the directory for a node's logs.
///
/// First visit (`visit <= 1`): `{run_dir}/nodes/{node_id}`
/// Subsequent visits: `{run_dir}/nodes/{node_id}-visit_{visit}`
pub fn node_dir(run_dir: &Path, node_id: &str, visit: usize) -> PathBuf {
    if visit <= 1 {
        run_dir.join("nodes").join(node_id)
    } else {
        run_dir
            .join("nodes")
            .join(format!("{node_id}-visit_{visit}"))
    }
}

/// Read the workflow visit ordinal from context.
///
/// The raw context value is `0` when unset; workflow execution code treats
/// missing counts as the first visit for stage/log naming.
pub fn visit_from_context(context: &Context) -> usize {
    context.node_visit_count().max(1)
}

/// Write status.json for a completed node into {`run_dir}/nodes/{node_id}/status.json`.
pub(crate) fn write_node_status(run_dir: &Path, node_id: &str, visit: usize, outcome: &Outcome) {
    let node_dir = node_dir(run_dir, node_id, visit);
    let _ = std::fs::create_dir_all(&node_dir);
    let status = serde_json::json!({
        "status": outcome.status.to_string(),
        "notes": outcome.notes,
        "failure_reason": outcome.failure_reason(),
        "timestamp": Utc::now().to_rfc3339(),
    });
    if let Ok(json) = serde_json::to_string_pretty(&status) {
        let _ = std::fs::write(node_dir.join("status.json"), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::context::Context;

    #[test]
    fn visit_from_context_defaults_to_first_visit() {
        let ctx = Context::new();
        assert_eq!(visit_from_context(&ctx), 1);
    }

    #[test]
    fn visit_from_context_preserves_stored_visit() {
        let ctx = Context::new();
        ctx.set(
            crate::context::keys::INTERNAL_NODE_VISIT_COUNT,
            serde_json::json!(3),
        );
        assert_eq!(visit_from_context(&ctx), 3);
    }

    #[test]
    fn node_dir_first_visit() {
        let root = Path::new("/tmp/logs");
        assert_eq!(node_dir(root, "work", 1), root.join("nodes").join("work"));
    }

    #[test]
    fn node_dir_second_visit() {
        let root = Path::new("/tmp/logs");
        assert_eq!(
            node_dir(root, "work", 2),
            root.join("nodes").join("work-visit_2")
        );
    }

    #[test]
    fn node_dir_fifth_visit() {
        let root = Path::new("/tmp/logs");
        assert_eq!(
            node_dir(root, "work", 5),
            root.join("nodes").join("work-visit_5")
        );
    }
}
