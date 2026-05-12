mod all_conditional_edges;
mod backend_valid;
mod condition_syntax;
mod direction_valid;
mod edge_target_exists;
mod exit_no_outgoing;
mod fidelity_valid;
mod freeform_edge_count;
mod goal_gate_has_retry;
mod import_error;
mod model_support;
mod node_model_known;
mod orphan_custom_outcome;
mod prompt_on_llm_nodes;
mod random_selection_no_conditions;
mod reachability;
mod reserved_keyword_node_id;
mod retry_target_exists;
mod script_absolute_cd;
mod selection_valid;
mod start_no_incoming;
mod start_node;
mod stylesheet_model_known;
mod stylesheet_syntax;
mod terminal_node;
#[cfg(test)]
pub(crate) mod test_support;
mod thread_id_requires_fidelity_full;
mod type_known;
mod unresolved_file_ref;

use crate::LintRule;

/// Returns all built-in lint rules.
#[must_use]
pub fn built_in_rules() -> Vec<Box<dyn LintRule>> {
    vec![
        start_node::rule(),
        terminal_node::rule(),
        reachability::rule(),
        edge_target_exists::rule(),
        start_no_incoming::rule(),
        exit_no_outgoing::rule(),
        condition_syntax::rule(),
        stylesheet_syntax::rule(),
        type_known::rule(),
        backend_valid::rule(),
        fidelity_valid::rule(),
        retry_target_exists::rule(),
        goal_gate_has_retry::rule(),
        prompt_on_llm_nodes::rule(),
        freeform_edge_count::rule(),
        direction_valid::rule(),
        reserved_keyword_node_id::rule(),
        all_conditional_edges::rule(),
        orphan_custom_outcome::rule(),
        script_absolute_cd::rule(),
        stylesheet_model_known::rule(),
        node_model_known::rule(),
        import_error::rule(),
        unresolved_file_ref::rule(),
        thread_id_requires_fidelity_full::rule(),
        selection_valid::rule(),
        random_selection_no_conditions::rule(),
    ]
}
