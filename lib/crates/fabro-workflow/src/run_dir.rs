use std::path::{Path, PathBuf};

use crate::context::Context;

/// Return the directory for a node's logs.
///
/// First visit (`visit <= 1`): `{run_dir}/nodes/{node_id}`
/// Subsequent visits: `{run_dir}/nodes/{node_id}-visit_{visit}`
pub(crate) fn node_dir(run_dir: &Path, node_id: &str, visit: usize) -> PathBuf {
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
pub(crate) fn visit_from_context(context: &Context) -> usize {
    context.node_visit_count().max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

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
