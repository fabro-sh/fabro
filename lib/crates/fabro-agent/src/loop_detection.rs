use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::history::History;
use crate::types::Turn;

fn tool_call_signature(name: &str, arguments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    let args_str = arguments.to_string();
    args_str.hash(&mut hasher);
    hasher.finish()
}

fn extract_signatures_from_assistant(turn: &Turn) -> Vec<u64> {
    let Turn::Assistant { tool_calls, .. } = turn else {
        return vec![];
    };
    tool_calls
        .iter()
        .map(|tc| tool_call_signature(&tc.name, &tc.arguments))
        .collect()
}

#[must_use]
pub fn detect_loop(history: &History, window_size: usize) -> bool {
    // Extract tool call signatures from the last N assistant turns that have tool
    // calls
    let turns = history.turns();
    let mut signatures: Vec<u64> = Vec::new();

    // Walk backwards and collect signatures from assistant turns with tool calls
    let mut count = 0;
    for turn in turns.iter().rev() {
        if count >= window_size {
            break;
        }
        let sigs = extract_signatures_from_assistant(turn);
        if !sigs.is_empty() {
            // Combine all tool call signatures for this turn into a single signature
            let mut hasher = DefaultHasher::new();
            for sig in &sigs {
                sig.hash(&mut hasher);
            }
            signatures.push(hasher.finish());
            count += 1;
        }
    }

    // Signatures are in reverse order; reverse to chronological
    signatures.reverse();

    if signatures.len() < 2 {
        return false;
    }

    // Check repeating patterns of length 1, 2, 3
    for pattern_len in 1..=3 {
        if signatures.len() < pattern_len * 2 {
            continue;
        }
        if is_repeating_pattern(&signatures, pattern_len) {
            return true;
        }
    }

    false
}

fn is_repeating_pattern(signatures: &[u64], pattern_len: usize) -> bool {
    if signatures.len() < pattern_len * 2 {
        return false;
    }

    let pattern = &signatures[signatures.len() - pattern_len..];

    // Check ALL preceding groups in window match, not just the last 2
    let num_groups = signatures.len() / pattern_len;
    if num_groups < 2 {
        return false;
    }

    // Walk backwards through all complete groups
    let groups_start = signatures.len() - (num_groups * pattern_len);
    for group_idx in 0..num_groups - 1 {
        let start = groups_start + group_idx * pattern_len;
        let group = &signatures[start..start + pattern_len];
        if group != pattern {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use fabro_llm::types::{TokenCounts, ToolCall};

    use super::*;

    fn assistant_with_tool(name: &str, args: serde_json::Value) -> Turn {
        Turn::Assistant {
            content: String::new(),
            tool_calls: vec![ToolCall::new("call_1", name, args)],
            provider_parts: vec![],
            usage: Box::new(TokenCounts::default()),
            response_id: "resp".into(),
            timestamp: SystemTime::now(),
        }
    }

    #[test]
    fn too_few_turns_returns_false() {
        let history = History::default();
        assert!(!detect_loop(&history, 10));
    }

    #[test]
    fn single_turn_returns_false() {
        let mut history = History::default();
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        assert!(!detect_loop(&history, 10));
    }

    #[test]
    fn pattern_1_repeating_detected() {
        let mut history = History::default();
        // Same tool call repeated 3 times
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        assert!(detect_loop(&history, 10));
    }

    #[test]
    fn pattern_2_repeating_detected() {
        let mut history = History::default();
        // A-B-A-B pattern
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "read_file",
            serde_json::json!({"path": "foo.rs"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "read_file",
            serde_json::json!({"path": "foo.rs"}),
        ));
        assert!(detect_loop(&history, 10));
    }

    #[test]
    fn pattern_3_repeating_detected() {
        let mut history = History::default();
        // A-B-C-A-B-C pattern
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "read_file",
            serde_json::json!({"path": "a.rs"}),
        ));
        history.push(assistant_with_tool(
            "grep",
            serde_json::json!({"pattern": "fn"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "read_file",
            serde_json::json!({"path": "a.rs"}),
        ));
        history.push(assistant_with_tool(
            "grep",
            serde_json::json!({"pattern": "fn"}),
        ));
        assert!(detect_loop(&history, 10));
    }

    #[test]
    fn non_repeating_returns_false() {
        let mut history = History::default();
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "read_file",
            serde_json::json!({"path": "a.rs"}),
        ));
        history.push(assistant_with_tool(
            "grep",
            serde_json::json!({"pattern": "fn"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "cat"}),
        ));
        assert!(!detect_loop(&history, 10));
    }

    #[test]
    fn same_name_different_args_are_different() {
        let mut history = History::default();
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "pwd"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "cat"}),
        ));
        assert!(!detect_loop(&history, 10));
    }

    #[test]
    fn tool_call_signature_same_input_same_output() {
        let sig1 = tool_call_signature("shell", &serde_json::json!({"cmd": "ls"}));
        let sig2 = tool_call_signature("shell", &serde_json::json!({"cmd": "ls"}));
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn tool_call_signature_different_name_different_output() {
        let sig1 = tool_call_signature("shell", &serde_json::json!({"cmd": "ls"}));
        let sig2 = tool_call_signature("read_file", &serde_json::json!({"cmd": "ls"}));
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn tool_call_signature_different_args_different_output() {
        let sig1 = tool_call_signature("shell", &serde_json::json!({"cmd": "ls"}));
        let sig2 = tool_call_signature("shell", &serde_json::json!({"cmd": "pwd"}));
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn user_turns_are_ignored() {
        let mut history = History::default();
        history.push(Turn::User {
            content: "hello".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::User {
            content: "hello".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::User {
            content: "hello".into(),
            timestamp: SystemTime::now(),
        });
        assert!(!detect_loop(&history, 10));
    }

    #[test]
    fn window_size_limits_lookback() {
        let mut history = History::default();
        // Add non-repeating turns first
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "unique1"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "unique2"}),
        ));
        // Then repeating turns
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        history.push(assistant_with_tool(
            "shell",
            serde_json::json!({"cmd": "ls"}),
        ));
        // With window=2, we only see the last 2 which are repeating
        assert!(detect_loop(&history, 2));
    }
}
