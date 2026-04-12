use crate::context::Context;

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
}
