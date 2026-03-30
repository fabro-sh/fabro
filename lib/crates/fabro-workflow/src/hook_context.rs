use fabro_graphviz::graph::types::Node as GvNode;
use fabro_hooks::HookContext;

/// Populate node-related fields on a `HookContext` from a graph node.
pub(crate) fn set_hook_node(ctx: &mut HookContext, node: &GvNode) {
    ctx.node_id = Some(node.id.clone());
    ctx.node_label = Some(node.label().to_string());
    ctx.handler_type = node.handler_type().map(String::from);
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};
    use fabro_hooks::HookEvent;
    use fabro_types::fixtures;

    use super::*;

    #[test]
    fn set_hook_node_populates_hook_context_fields() {
        let mut node = Node::new("approve");
        node.attrs.insert(
            "label".to_string(),
            AttrValue::String("Approve PR".to_string()),
        );
        node.attrs
            .insert("type".to_string(), AttrValue::String("human".to_string()));

        let mut ctx = HookContext::new(HookEvent::StageStart, fixtures::RUN_1, "graph".into());
        set_hook_node(&mut ctx, &node);

        assert_eq!(ctx.node_id.as_deref(), Some("approve"));
        assert_eq!(ctx.node_label.as_deref(), Some("Approve PR"));
        assert_eq!(ctx.handler_type.as_deref(), Some("human"));
    }
}
