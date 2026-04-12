use std::time::Duration;

use fabro_core::retry::{BackoffPolicy, RetryPolicy};
use fabro_graphviz::graph::types::{Graph as GvGraph, Node as GvNode};

const DEFAULT_BACKOFF: BackoffPolicy = BackoffPolicy {
    initial_delay: Duration::from_millis(5_000),
    factor:        2.0,
    max_delay:     Duration::from_millis(60_000),
    jitter:        true,
};

/// Build a retry policy from node and graph attributes.
/// If the node has a `retry_policy` attribute naming a preset, use that.
/// Otherwise, fall back to `max_retries` / graph default.
pub(crate) fn build_retry_policy(node: &GvNode, graph: &GvGraph) -> RetryPolicy {
    if let Some(preset) = node.retry_policy() {
        if let Some(policy) = preset_retry_policy(preset) {
            return policy;
        }
    }

    let max_retries = node
        .max_retries()
        .unwrap_or_else(|| graph.default_max_retries());
    let max_attempts = u32::try_from(max_retries + 1).unwrap_or(1).max(1);

    RetryPolicy {
        max_attempts,
        backoff: DEFAULT_BACKOFF,
    }
}

fn preset_retry_policy(preset: &str) -> Option<RetryPolicy> {
    match preset {
        "none" => Some(RetryPolicy {
            max_attempts: 1,
            backoff:      DEFAULT_BACKOFF,
        }),
        "standard" => Some(RetryPolicy {
            max_attempts: 5,
            backoff:      DEFAULT_BACKOFF,
        }),
        "aggressive" => Some(RetryPolicy {
            max_attempts: 5,
            backoff:      BackoffPolicy {
                initial_delay: Duration::from_millis(500),
                ..DEFAULT_BACKOFF
            },
        }),
        "linear" => Some(RetryPolicy {
            max_attempts: 3,
            backoff:      BackoffPolicy {
                initial_delay: Duration::from_millis(500),
                factor: 1.0,
                ..DEFAULT_BACKOFF
            },
        }),
        "patient" => Some(RetryPolicy {
            max_attempts: 3,
            backoff:      BackoffPolicy {
                initial_delay: Duration::from_millis(2_000),
                factor: 3.0,
                ..DEFAULT_BACKOFF
            },
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fabro_graphviz::graph::{AttrValue, Graph, Node};

    use super::*;

    #[test]
    fn build_retry_policy_from_node() {
        let mut node = Node::new("n");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 4);
    }

    #[test]
    fn build_retry_policy_from_graph_default() {
        let node = Node::new("n");
        let mut graph = Graph::new("test");
        graph
            .attrs
            .insert("default_max_retries".to_string(), AttrValue::Integer(2));
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 3);
    }

    #[test]
    fn build_retry_policy_no_attrs_uses_graph_default_0() {
        let node = Node::new("n");
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 1);
    }

    #[test]
    fn build_retry_policy_from_retry_policy_attr() {
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("aggressive".to_string()),
        );
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(500));
    }

    #[test]
    fn build_retry_policy_fallback_when_no_retry_policy_attr() {
        let mut node = Node::new("n");
        node.attrs
            .insert("max_retries".to_string(), AttrValue::Integer(3));
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 4);
        assert_eq!(policy.backoff.initial_delay, Duration::from_millis(5_000));
    }

    #[test]
    fn build_retry_policy_all_presets() {
        let presets = [
            ("none", 1u32),
            ("standard", 5),
            ("aggressive", 5),
            ("linear", 3),
            ("patient", 3),
        ];
        let graph = Graph::new("test");
        let (name, expected) = presets[0];
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[1];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[2];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[3];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);

        let (name, expected) = presets[4];
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String(name.to_string()),
        );
        assert_eq!(build_retry_policy(&node, &graph).max_attempts, expected);
    }

    #[test]
    fn build_retry_policy_unknown_preset_falls_back() {
        let mut node = Node::new("n");
        node.attrs.insert(
            "retry_policy".to_string(),
            AttrValue::String("unknown_preset".to_string()),
        );
        let graph = Graph::new("test");
        let policy = build_retry_policy(&node, &graph);
        assert_eq!(policy.max_attempts, 1);
    }
}
