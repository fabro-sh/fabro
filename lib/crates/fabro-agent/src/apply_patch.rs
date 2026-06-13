// Copyright 2026 OpenAI
// SPDX-License-Identifier: Apache-2.0
// Ported from openai/codex codex-rs/apply-patch at 932f72c225.

use std::fmt::Write as _;
use std::sync::Arc;

use fabro_llm::types::ToolDefinition;

use crate::sandbox::Sandbox;
use crate::tool_registry::{RegisteredTool, ToolSource};

const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("apply_patch.lark");

fn apply_patch_lark_grammar_definition() -> String {
    APPLY_PATCH_LARK_GRAMMAR
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Remove(String),
    Add(String),
    Context(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub context_line: String,
    pub changes:      Vec<Change>,
    pub end_of_file:  bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOperation {
    Add {
        path:    String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path:     String,
        new_path: Option<String>,
        hunks:    Vec<Hunk>,
    },
}

fn is_hunk_start(line: &str) -> bool {
    line == "@@" || line.starts_with("@@ ")
}

fn extract_context_line(line: &str) -> String {
    if line == "@@" {
        String::new()
    } else {
        let raw = line.strip_prefix("@@ ").unwrap_or(line);
        raw.strip_suffix(" @@").unwrap_or(raw).trim().to_string()
    }
}

/// Parses Codex apply_patch text into a list of patch operations.
///
/// # Errors
/// Returns an error if the patch format is invalid.
pub fn parse_apply_patch(text: &str) -> Result<Vec<PatchOperation>, String> {
    let lines: Vec<&str> = text.trim().lines().collect();
    let lines = patch_lines_with_valid_boundaries(&lines)?;
    let mut ops = Vec::new();
    let mut i = 0;

    i += 1;

    while i < lines.len() {
        let line = lines[i].trim();

        if line == "*** End Patch" {
            break;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = path.to_string();
            i += 1;
            let mut content = String::new();
            let mut add_lines = 0;
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("*** ") {
                    break;
                }
                if let Some(text_line) = l.strip_prefix('+') {
                    content.push_str(text_line);
                    content.push('\n');
                    add_lines += 1;
                } else {
                    return Err(format!("Expected '+' prefix in Add File block, got: {l}"));
                }
                i += 1;
            }
            if add_lines == 0 {
                return Err(format!("Add file hunk for path '{path}' is empty"));
            }
            ops.push(PatchOperation::Add { path, content });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(PatchOperation::Delete {
                path: path.to_string(),
            });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = path.to_string();
            i += 1;

            // Check for *** Move to:
            let new_path = if i < lines.len() {
                if let Some(np) = lines[i].trim().strip_prefix("*** Move to: ") {
                    i += 1;
                    Some(np.to_string())
                } else {
                    None
                }
            } else {
                None
            };

            let mut hunks = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("*** ") && !is_hunk_start(l) {
                    break;
                }
                if is_hunk_start(l) {
                    // Consume stacked @@ lines, keeping the last context
                    let mut context_line = extract_context_line(l);
                    i += 1;
                    while i < lines.len() && is_hunk_start(lines[i]) {
                        context_line = extract_context_line(lines[i]);
                        i += 1;
                    }

                    let mut changes = Vec::new();
                    while i < lines.len() {
                        let cl = lines[i];
                        if cl.starts_with("*** ") || is_hunk_start(cl) {
                            break;
                        }
                        if let Some(removed) = cl.strip_prefix('-') {
                            changes.push(Change::Remove(removed.to_string()));
                        } else if let Some(added) = cl.strip_prefix('+') {
                            changes.push(Change::Add(added.to_string()));
                        } else if let Some(ctx) = cl.strip_prefix(' ') {
                            changes.push(Change::Context(ctx.to_string()));
                        } else if cl.is_empty() {
                            changes.push(Change::Context(String::new()));
                        } else {
                            return Err(format!(
                                "Unexpected line in hunk (expected +, -, or space prefix): {cl}"
                            ));
                        }
                        i += 1;
                    }

                    // Check for *** End of File marker
                    let end_of_file = if i < lines.len() && lines[i].trim() == "*** End of File" {
                        i += 1;
                        true
                    } else {
                        false
                    };

                    hunks.push(Hunk {
                        context_line,
                        changes,
                        end_of_file,
                    });
                } else {
                    return Err(format!("Expected @@ context line, got: {l}"));
                }
            }
            if hunks.is_empty() {
                return Err(format!("Update file hunk for path '{path}' is empty"));
            }
            ops.push(PatchOperation::Update {
                path,
                new_path,
                hunks,
            });
        } else {
            return Err(format!("Unexpected line in patch: {line}"));
        }
    }

    Ok(ops)
}

fn patch_lines_with_valid_boundaries<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], String> {
    match check_patch_boundaries_strict(lines) {
        Ok(()) => Ok(lines),
        Err(original_error) => {
            if let [first, .., last] = lines {
                if (*first == "<<EOF" || *first == "<<'EOF'" || *first == "<<\"EOF\"")
                    && last.ends_with("EOF")
                    && lines.len() >= 4
                {
                    let inner = &lines[1..lines.len() - 1];
                    check_patch_boundaries_strict(inner)?;
                    return Ok(inner);
                }
            }
            Err(original_error)
        }
    }
}

fn check_patch_boundaries_strict(lines: &[&str]) -> Result<(), String> {
    let first_line = lines.first().map(|line| line.trim());
    let last_line = lines.last().map(|line| line.trim());

    match (first_line, last_line) {
        (Some("*** Begin Patch"), Some("*** End Patch")) => Ok(()),
        (Some(first), _) if first != "*** Begin Patch" => {
            Err("The first line of the patch must be '*** Begin Patch'".to_string())
        }
        _ => Err("The last line of the patch must be '*** End Patch'".to_string()),
    }
}

/// Applies a list of patch operations using the given sandbox.
///
/// # Errors
/// Returns an error if any file operation fails.
pub async fn apply_patch_operations(
    ops: &[PatchOperation],
    env: &dyn Sandbox,
) -> Result<String, String> {
    if ops.is_empty() {
        return Err("No files were modified.".to_string());
    }

    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for op in ops {
        match op {
            PatchOperation::Add { path, content } => {
                env.write_file(path, content).await.map_err(|e| {
                    format!("Failed to write file {path}: {}", e.display_with_causes())
                })?;
                added.push(path.clone());
            }
            PatchOperation::Delete { path } => {
                if !env.file_exists(path).await.map_err(|e| {
                    format!("Failed to delete file {path}: {}", e.display_with_causes())
                })? {
                    return Err(format!("Failed to delete file {path}: file does not exist"));
                }
                env.delete_file(path).await.map_err(|e| {
                    format!("Failed to delete file {path}: {}", e.display_with_causes())
                })?;
                deleted.push(path.clone());
            }
            PatchOperation::Update {
                path,
                new_path,
                hunks,
            } => {
                let original = env.read_file_text(path).await.map_err(|e| {
                    format!(
                        "Failed to read file to update {path}: {}",
                        e.display_with_causes()
                    )
                })?;
                let updated = apply_hunks(path, &original, hunks)?;
                let dest = new_path.as_deref().unwrap_or(path);
                env.write_file(dest, &updated).await.map_err(|e| {
                    format!("Failed to write file {dest}: {}", e.display_with_causes())
                })?;
                if new_path.is_some() {
                    env.delete_file(path).await.map_err(|e| {
                        format!(
                            "Failed to remove original {path}: {}",
                            e.display_with_causes()
                        )
                    })?;
                }
                modified.push(dest.to_string());
            }
        }
    }

    Ok(format_summary(&added, &modified, &deleted))
}

fn normalize_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' => ' ',
        other => other,
    }
}

fn normalize_unicode(s: &str) -> String {
    s.trim().chars().map(normalize_char).collect()
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| lines[i + offset].trim_end() == pat.trim_end())
        {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| lines[i + offset].trim() == pat.trim())
        {
            return Some(i);
        }
    }

    (search_start..=lines.len().saturating_sub(pattern.len())).find(|&i| {
        pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| normalize_unicode(&lines[i + offset]) == normalize_unicode(pat))
    })
}

fn apply_hunks(path: &str, content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut original_lines: Vec<String> = content.split('\n').map(String::from).collect();
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, hunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok(new_lines.join("\n"))
}

fn compute_replacements(
    original_lines: &[String],
    path: &str,
    hunks: &[Hunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, String> {
    let mut replacements = Vec::new();
    let mut line_index = 0;

    for hunk in hunks {
        if !hunk.context_line.is_empty() {
            if let Some(index) = seek_sequence(
                original_lines,
                std::slice::from_ref(&hunk.context_line),
                line_index,
                false,
            ) {
                line_index = index + 1;
            } else {
                return Err(format!(
                    "Failed to find context '{}' in {path}",
                    hunk.context_line
                ));
            }
        }

        let mut old_lines = Vec::new();
        let mut new_lines = Vec::new();
        for change in &hunk.changes {
            match change {
                Change::Remove(line) => old_lines.push(line.clone()),
                Change::Add(line) => new_lines.push(line.clone()),
                Change::Context(line) => {
                    old_lines.push(line.clone());
                    new_lines.push(line.clone());
                }
            }
        }

        if old_lines.is_empty() {
            let insertion_index = original_lines.len();
            replacements.push((insertion_index, 0, new_lines));
            continue;
        }

        let mut pattern: &[String] = &old_lines;
        let mut new_slice: &[String] = &new_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, hunk.end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, hunk.end_of_file);
        }

        if let Some(start_index) = found {
            replacements.push((start_index, pattern.len(), new_slice.to_vec()));
            line_index = start_index + pattern.len();
        } else {
            return Err(format!(
                "Failed to find expected lines in {path}:\n{}",
                old_lines.join("\n")
            ));
        }
    }

    replacements.sort_by_key(|(start_index, _, _)| *start_index);
    Ok(replacements)
}

fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start_index, old_len, new_segment) in replacements.iter().rev() {
        for _ in 0..*old_len {
            if *start_index < lines.len() {
                lines.remove(*start_index);
            }
        }
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(*start_index + offset, new_line.clone());
        }
    }
    lines
}

fn format_summary(added: &[String], modified: &[String], deleted: &[String]) -> String {
    let mut output = String::from("Success. Updated the following files:\n");
    for path in added {
        let _ = writeln!(output, "A {path}");
    }
    for path in modified {
        let _ = writeln!(output, "M {path}");
    }
    for path in deleted {
        let _ = writeln!(output, "D {path}");
    }
    output
}

pub fn make_apply_patch_tool() -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition::custom(
            "apply_patch",
            "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
            serde_json::json!({
                "type": "grammar",
                "syntax": "lark",
                "definition": apply_patch_lark_grammar_definition(),
            }),
        ),
        executor:   Arc::new(|args, ctx| {
            Box::pin(async move {
                let patch_text = args
                    .as_str()
                    .ok_or_else(|| "apply_patch expects raw patch text".to_string())?;

                let ops = parse_apply_patch(patch_text)?;
                apply_patch_operations(&ops, ctx.env.as_ref()).await
            })
        }),
        source:     ToolSource::Native,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_llm::types::{
        ContentPart, FinishReason, Message as LlmMessage, Response, Role, TokenCounts, ToolCall,
    };
    use tokio::fs;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::LocalSandbox;
    use crate::test_support::MutableMockSandbox;
    use crate::tool_registry::ToolContext;

    #[test]
    fn parse_apply_patch_add_file() {
        let patch = "\
*** Begin Patch
*** Add File: src/new_file.rs
+fn main() {
+    println!(\"hello\");
+}
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0], PatchOperation::Add {
            path:    "src/new_file.rs".into(),
            content: "fn main() {\n    println!(\"hello\");\n}\n".into(),
        });
    }

    #[test]
    fn parse_apply_patch_delete_file() {
        let patch = "\
*** Begin Patch
*** Delete File: src/old_file.rs
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0], PatchOperation::Delete {
            path: "src/old_file.rs".into(),
        });
    }

    #[test]
    fn parse_apply_patch_update_file() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@ fn hello() @@
-    println!(\"old\");
+    println!(\"new\");
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOperation::Update {
                path,
                new_path,
                hunks,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(*new_path, None);
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].context_line, "fn hello()");
                assert!(!hunks[0].end_of_file);
                assert_eq!(hunks[0].changes.len(), 2);
                assert_eq!(
                    hunks[0].changes[0],
                    Change::Remove("    println!(\"old\");".into())
                );
                assert_eq!(
                    hunks[0].changes[1],
                    Change::Add("    println!(\"new\");".into())
                );
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn parse_apply_patch_multi_operation() {
        let patch = "\
*** Begin Patch
*** Add File: src/a.rs
+// file a
*** Delete File: src/b.rs
*** Update File: src/c.rs
@@ fn main() @@
-    old_call();
+    new_call();
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], PatchOperation::Add { .. }));
        assert!(matches!(&ops[1], PatchOperation::Delete { .. }));
        assert!(matches!(&ops[2], PatchOperation::Update { .. }));
    }

    #[test]
    fn parse_apply_patch_bare_at_at_hunk() {
        let patch = "\
*** Begin Patch
*** Update File: src/game.py
@@
-from src.cards import Suit
+from src.cards import Card, Suit
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOperation::Update { path, hunks, .. } => {
                assert_eq!(path, "src/game.py");
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].context_line, "");
                assert_eq!(hunks[0].changes.len(), 2);
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn parse_apply_patch_multiple_bare_at_at_hunks() {
        let patch = "\
*** Begin Patch
*** Update File: src/game.py
@@
-from src.cards import Suit
+from src.cards import Card, Suit
@@
-    stock: list = field(default_factory=list)
+    stock: list[Card] = field(default_factory=list)
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks.len(), 2);
                assert_eq!(hunks[0].context_line, "");
                assert_eq!(hunks[1].context_line, "");
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[tokio::test]
    async fn apply_patch_bare_at_at_update() {
        let mut files = HashMap::new();
        files.insert(
            "src/game.py".to_string(),
            "from src.cards import Suit\nfrom src.piles import Pile\n\nclass GameState:\n    stock: list = field(default_factory=list)\n    waste: list = field(default_factory=list)".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let ops = vec![PatchOperation::Update {
            path:     "src/game.py".into(),
            new_path: None,
            hunks:    vec![
                Hunk {
                    context_line: String::new(),
                    end_of_file:  false,
                    changes:      vec![
                        Change::Remove("from src.cards import Suit".into()),
                        Change::Add("from src.cards import Card, Suit".into()),
                    ],
                },
                Hunk {
                    context_line: String::new(),
                    end_of_file:  false,
                    changes:      vec![
                        Change::Remove("    stock: list = field(default_factory=list)".into()),
                        Change::Remove("    waste: list = field(default_factory=list)".into()),
                        Change::Add("    stock: list[Card] = field(default_factory=list)".into()),
                        Change::Add("    waste: list[Card] = field(default_factory=list)".into()),
                    ],
                },
            ],
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("M src/game.py"));

        let content = env.read_file_text("src/game.py").await.unwrap();
        assert!(content.contains("from src.cards import Card, Suit"));
        assert!(!content.contains("from src.cards import Suit\n"));
        assert!(content.contains("stock: list[Card]"));
        assert!(content.contains("waste: list[Card]"));
        assert!(content.contains("from src.piles import Pile"));
    }

    #[test]
    fn parse_apply_patch_mixed_bare_and_contextual_hunks() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@ fn setup() @@
-    old_setup();
+    new_setup();
@@
-    old_teardown();
+    new_teardown();
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks.len(), 2);
                assert_eq!(hunks[0].context_line, "fn setup()");
                assert_eq!(hunks[1].context_line, "");
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn parse_apply_patch_bare_at_at_with_context_lines() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
 fn unchanged() {
-    old_line();
+    new_line();
 }
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].context_line, "");
                assert_eq!(hunks[0].changes.len(), 4);
                assert_eq!(
                    hunks[0].changes[0],
                    Change::Context("fn unchanged() {".into())
                );
                assert_eq!(
                    hunks[0].changes[1],
                    Change::Remove("    old_line();".into())
                );
                assert_eq!(hunks[0].changes[2], Change::Add("    new_line();".into()));
                assert_eq!(hunks[0].changes[3], Change::Context("}".into()));
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn parse_apply_patch_bare_at_at_add_only_appends_to_file() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
+new_line();
*** End Patch";

        // Parsing succeeds — the hunk is structurally valid
        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks[0].context_line, "");
                assert_eq!(hunks[0].changes.len(), 1);
                assert_eq!(hunks[0].changes[0], Change::Add("new_line();".into()));
            }
            _ => panic!("Expected Update operation"),
        }

        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                let result = apply_hunks("src/lib.rs", "fn main() {}\n", hunks).unwrap();
                assert_eq!(result, "fn main() {}\nnew_line();\n");
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[tokio::test]
    async fn apply_patch_bare_at_at_with_context_lines() {
        let mut files = HashMap::new();
        files.insert(
            "src/lib.rs".to_string(),
            "fn unchanged() {\n    old_line();\n}".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let ops = vec![PatchOperation::Update {
            path:     "src/lib.rs".into(),
            new_path: None,
            hunks:    vec![Hunk {
                context_line: String::new(),
                end_of_file:  false,
                changes:      vec![
                    Change::Context("fn unchanged() {".into()),
                    Change::Remove("    old_line();".into()),
                    Change::Add("    new_line();".into()),
                    Change::Context("}".into()),
                ],
            }],
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("M src/lib.rs"));

        let content = env.read_file_text("src/lib.rs").await.unwrap();
        assert_eq!(content, "fn unchanged() {\n    new_line();\n}\n");
    }

    #[tokio::test]
    async fn apply_patch_mixed_bare_and_contextual_hunks() {
        let mut files = HashMap::new();
        files.insert(
            "src/lib.rs".to_string(),
            "import foo\nimport bar\n\ndef setup():\n    old_setup()\n\ndef teardown():\n    old_teardown()\n".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let ops = vec![PatchOperation::Update {
            path:     "src/lib.rs".into(),
            new_path: None,
            hunks:    vec![
                Hunk {
                    context_line: "def setup():".into(),
                    end_of_file:  false,
                    changes:      vec![
                        Change::Remove("    old_setup()".into()),
                        Change::Add("    new_setup()".into()),
                    ],
                },
                Hunk {
                    context_line: String::new(),
                    end_of_file:  false,
                    changes:      vec![
                        Change::Remove("    old_teardown()".into()),
                        Change::Add("    new_teardown()".into()),
                    ],
                },
            ],
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("M src/lib.rs"));

        let content = env.read_file_text("src/lib.rs").await.unwrap();
        assert!(content.contains("new_setup()"));
        assert!(content.contains("new_teardown()"));
        assert!(!content.contains("old_setup()"));
        assert!(!content.contains("old_teardown()"));
    }

    #[tokio::test]
    async fn apply_patch_add_file() {
        let env = MutableMockSandbox::new(HashMap::new());
        let ops = vec![PatchOperation::Add {
            path:    "src/new.rs".into(),
            content: "fn new() {}".into(),
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("A src/new.rs"));

        let content = env.read_file_text("src/new.rs").await.unwrap();
        assert_eq!(content, "fn new() {}");
    }

    #[tokio::test]
    async fn apply_patch_update_file() {
        let mut files = HashMap::new();
        files.insert(
            "src/lib.rs".to_string(),
            "fn hello() {\n    println!(\"old\");\n}".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let ops = vec![PatchOperation::Update {
            path:     "src/lib.rs".into(),
            new_path: None,
            hunks:    vec![Hunk {
                context_line: "fn hello() {".into(),
                end_of_file:  false,
                changes:      vec![
                    Change::Remove("    println!(\"old\");".into()),
                    Change::Add("    println!(\"new\");".into()),
                ],
            }],
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("M src/lib.rs"));

        let content = env.read_file_text("src/lib.rs").await.unwrap();
        assert!(content.contains("println!(\"new\")"));
        assert!(!content.contains("println!(\"old\")"));
    }

    #[tokio::test]
    async fn apply_patch_updates_raw_local_file_without_line_number_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("src/lib.rs");
        fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        fs::write(&path, "fn hello() {\n    println!(\"old\");\n}\n")
            .await
            .unwrap();
        let env = LocalSandbox::new(dir.path().to_path_buf());
        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@
-    println!(\"old\");
+    println!(\"new\");
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let result = apply_patch_operations(&ops, &env).await.unwrap();

        assert_eq!(
            result,
            "Success. Updated the following files:\nM src/lib.rs\n"
        );
        assert_eq!(
            fs::read_to_string(&path).await.unwrap(),
            "fn hello() {\n    println!(\"new\");\n}\n"
        );
    }

    #[test]
    fn apply_patch_tool_definition_is_custom_freeform() {
        let tool = make_apply_patch_tool();

        assert_eq!(tool.definition.name, "apply_patch");
        assert!(tool.definition.is_custom());
        assert_eq!(
            tool.definition
                .custom_format()
                .and_then(|format| format.get("type")),
            Some(&serde_json::json!("grammar"))
        );
        assert_eq!(
            tool.definition
                .custom_format()
                .and_then(|format| format.get("syntax")),
            Some(&serde_json::json!("lark"))
        );
    }

    #[tokio::test]
    async fn apply_patch_tool_executor_accepts_raw_patch_string() {
        let env = Arc::new(MutableMockSandbox::new(HashMap::new()));
        let tool = make_apply_patch_tool();
        let patch = "\
*** Begin Patch
*** Add File: hello.txt
+hello
*** End Patch
";
        let ctx = ToolContext {
            env,
            cancel: CancellationToken::new(),
            tool_env_provider: None,
            session_id: None,
            root_session_id: None,
            tool_call_id: None,
            agent_event_emitter: None,
        };

        let output = (tool.executor)(serde_json::json!(patch), ctx)
            .await
            .expect("raw custom patch should apply");

        assert_eq!(
            output,
            "Success. Updated the following files:\nA hello.txt\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_add_overwrites_existing_file_with_codex_summary() {
        let mut files = HashMap::new();
        files.insert("duplicate.txt".to_string(), "old content\n".to_string());
        let env = MutableMockSandbox::new(files);
        let patch = "\
*** Begin Patch
*** Add File: duplicate.txt
+new content
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let result = apply_patch_operations(&ops, &env).await.unwrap();

        assert_eq!(
            result,
            "Success. Updated the following files:\nA duplicate.txt\n"
        );
        assert_eq!(
            env.read_file_text("duplicate.txt").await.unwrap(),
            "new content\n"
        );
    }

    #[test]
    fn parse_update_file_hunk_rejects_empty_update() {
        let patch = "\
*** Begin Patch
*** Update File: empty.txt
*** End Patch";

        let err = parse_apply_patch(patch).expect_err("empty update hunk should be rejected");

        assert!(err.contains("Update file hunk for path 'empty.txt' is empty"));
    }

    #[tokio::test]
    async fn pure_addition_update_hunk_appends_before_final_newline() {
        let mut files = HashMap::new();
        files.insert("insert_only.txt".to_string(), "alpha\nomega\n".to_string());
        let env = MutableMockSandbox::new(files);
        let patch = "\
*** Begin Patch
*** Update File: insert_only.txt
@@
+inserted
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let result = apply_patch_operations(&ops, &env).await.unwrap();

        assert_eq!(
            result,
            "Success. Updated the following files:\nM insert_only.txt\n"
        );
        assert_eq!(
            env.read_file_text("insert_only.txt").await.unwrap(),
            "alpha\nomega\ninserted\n"
        );
    }

    #[tokio::test]
    async fn pure_addition_update_hunk_uses_raw_local_file_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("insert_only.txt");
        fs::write(&path, "alpha\nomega\n").await.unwrap();
        let env = LocalSandbox::new(dir.path().to_path_buf());
        let patch = "\
*** Begin Patch
*** Update File: insert_only.txt
@@
+inserted
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let result = apply_patch_operations(&ops, &env).await.unwrap();

        assert_eq!(
            result,
            "Success. Updated the following files:\nM insert_only.txt\n"
        );
        assert_eq!(
            fs::read_to_string(&path).await.unwrap(),
            "alpha\nomega\ninserted\n"
        );
    }

    #[tokio::test]
    async fn update_normalizes_missing_trailing_newline() {
        let mut files = HashMap::new();
        files.insert(
            "no_newline.txt".to_string(),
            "no newline at end".to_string(),
        );
        let env = MutableMockSandbox::new(files);
        let patch = "\
*** Begin Patch
*** Update File: no_newline.txt
@@
-no newline at end
+has newline now
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        apply_patch_operations(&ops, &env).await.unwrap();

        assert_eq!(
            env.read_file_text("no_newline.txt").await.unwrap(),
            "has newline now\n"
        );
    }

    #[test]
    fn parse_rejects_text_before_patch_envelope() {
        let patch = "\
please apply this
*** Begin Patch
*** Add File: hello.txt
+hello
*** End Patch";

        let err = parse_apply_patch(patch).expect_err("patch envelope must start on first line");

        assert!(err.contains("The first line of the patch must be '*** Begin Patch'"));
    }

    #[tokio::test]
    async fn apply_patch_error_reports_failed_context() {
        let mut files = HashMap::new();
        files.insert(
            "src/game.py".to_string(),
            "def real_fn():\n    pass".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let ops = vec![PatchOperation::Update {
            path:     "src/game.py".into(),
            new_path: None,
            hunks:    vec![Hunk {
                context_line: "def nonexistent():".into(),
                end_of_file:  false,
                changes:      vec![
                    Change::Remove("    old_body()".into()),
                    Change::Add("    new_body()".into()),
                ],
            }],
        }];

        let err = apply_patch_operations(&ops, &env).await.unwrap_err();
        assert_eq!(
            err,
            "Failed to find context 'def nonexistent():' in src/game.py"
        );
    }

    #[tokio::test]
    async fn update_missing_target_file_rejected() {
        let env = MutableMockSandbox::new(HashMap::new());
        let patch = "\
*** Begin Patch
*** Update File: missing.txt
@@
-old
+new
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let err = apply_patch_operations(&ops, &env).await.unwrap_err();

        assert!(err.contains("Failed to read file to update missing.txt"));
    }

    #[tokio::test]
    async fn delete_missing_target_file_rejected() {
        let env = MutableMockSandbox::new(HashMap::new());
        let patch = "\
*** Begin Patch
*** Delete File: missing.txt
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let err = apply_patch_operations(&ops, &env).await.unwrap_err();

        assert_eq!(
            err,
            "Failed to delete file missing.txt: file does not exist"
        );
    }

    // Phase 0: Forward-order hunk application

    #[test]
    fn apply_hunks_bare_at_at_searches_forward_from_previous_hunk() {
        let content = "def foo():\n    pass\n\ndef bar():\n    pass";
        let hunks = vec![
            Hunk {
                context_line: String::new(),
                end_of_file:  false,
                changes:      vec![
                    Change::Remove("    pass".into()),
                    Change::Add("    return 1".into()),
                ],
            },
            Hunk {
                context_line: String::new(),
                end_of_file:  false,
                changes:      vec![
                    Change::Remove("    pass".into()),
                    Change::Add("    return 2".into()),
                ],
            },
        ];
        let result = apply_hunks("example.py", content, &hunks).unwrap();
        assert!(result.contains("return 1"));
        assert!(result.contains("return 2"));
        assert!(!result.contains("    pass"));
    }

    // Phase 1: Context without trailing @@

    #[test]
    fn parse_apply_patch_context_without_trailing_markers() {
        let patch = "\
*** Begin Patch
*** Update File: src/hello.py
@@ def hello():
-    print(\"old\")
+    print(\"new\")
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks[0].context_line, "def hello():");
            }
            _ => panic!("Expected Update operation"),
        }
    }

    // Phase 2: Stacked @@ anchors

    #[test]
    fn parse_apply_patch_stacked_context_uses_last() {
        let patch = "\
*** Begin Patch
*** Update File: src/foo.py
@@ class Foo:
@@   def bar(self):
-        pass
+        return 42
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].context_line, "def bar(self):");
            }
            _ => panic!("Expected Update operation"),
        }
    }

    // Phase 3: *** End of File

    #[test]
    fn parse_apply_patch_end_of_file_marker() {
        let patch = "\
*** Begin Patch
*** Update File: src/lib.py
@@
-    pass
+    return 1
*** End of File
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks.len(), 1);
                assert!(hunks[0].end_of_file);
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn apply_hunks_end_of_file_searches_backward() {
        // Two functions with identical "pass" line — End of File matches the last one
        let content = "def foo():\n    pass\n\ndef bar():\n    pass";
        let hunks = vec![Hunk {
            context_line: String::new(),
            end_of_file:  true,
            changes:      vec![
                Change::Remove("    pass".into()),
                Change::Add("    return 99".into()),
            ],
        }];
        let result = apply_hunks("example.py", content, &hunks).unwrap();
        // First "pass" should be untouched, second should be replaced
        assert_eq!(
            result,
            "def foo():\n    pass\n\ndef bar():\n    return 99\n"
        );
    }

    // Phase 4: *** Move to:

    #[test]
    fn parse_apply_patch_move_to() {
        let patch = "\
*** Begin Patch
*** Update File: src/old.py
*** Move to: src/new.py
@@ def hello():
-    pass
+    return 1
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        match &ops[0] {
            PatchOperation::Update {
                path,
                new_path,
                hunks,
            } => {
                assert_eq!(path, "src/old.py");
                assert_eq!(*new_path, Some("src/new.py".to_string()));
                assert_eq!(hunks.len(), 1);
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[tokio::test]
    async fn apply_patch_move_to_renames_file() {
        let mut files = HashMap::new();
        files.insert(
            "src/old.py".to_string(),
            "def hello():\n    pass".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let ops = vec![PatchOperation::Update {
            path:     "src/old.py".into(),
            new_path: Some("src/new.py".into()),
            hunks:    vec![Hunk {
                context_line: "def hello():".into(),
                end_of_file:  false,
                changes:      vec![
                    Change::Remove("    pass".into()),
                    Change::Add("    return 1".into()),
                ],
            }],
        }];

        let result = apply_patch_operations(&ops, &env).await.unwrap();
        assert!(result.contains("M src/new.py"));

        // New path exists with updated content
        let content = env.read_file_text("src/new.py").await.unwrap();
        assert_eq!(content, "def hello():\n    return 1\n");

        // Old path is deleted
        let old = env.read_file_text("src/old.py").await;
        assert!(old.is_err());
    }

    // Phase 5: Fuzzy matching

    #[test]
    fn apply_hunks_prefers_exact_match_over_trimmed() {
        // Line 0 has leading spaces, line 1 is exact match
        let content = "  indented\nindented";
        let hunks = vec![Hunk {
            context_line: "indented".into(),
            end_of_file:  false,
            changes:      vec![Change::Add("extra".into())],
        }];
        let result = apply_hunks("example.txt", content, &hunks).unwrap();
        // Should match line 1 (exact), so "extra" inserted after "indented" (line 1)
        assert_eq!(result, "  indented\nindented\nextra\n");
    }

    #[test]
    fn apply_hunks_fuzzy_unicode_normalization() {
        let content = "print(\u{201C}hello\u{201D})";
        let hunks = vec![Hunk {
            context_line: "print(\"hello\")".into(),
            end_of_file:  false,
            changes:      vec![Change::Add("print(\"world\")".into())],
        }];
        let result = apply_hunks("example.py", content, &hunks).unwrap();
        // Original line preserved, new line added after
        assert!(result.contains("print(\u{201C}hello\u{201D})"));
        assert!(result.contains("print(\"world\")"));
    }

    // Phase 6: Heredoc stripping

    #[test]
    fn parse_apply_patch_strips_heredoc_wrapper() {
        let patch = "\
<<'EOF'
*** Begin Patch
*** Update File: src/lib.rs
@@ fn hello():
-    pass
+    return 1
*** End Patch
EOF";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOperation::Update { hunks, .. } => {
                assert_eq!(hunks[0].context_line, "fn hello():");
            }
            _ => panic!("Expected Update operation"),
        }
    }

    #[test]
    fn parse_apply_patch_strips_heredoc_unquoted() {
        let patch = "\
<<EOF
*** Begin Patch
*** Add File: src/a.rs
+// hello
*** End Patch
EOF";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], PatchOperation::Add { .. }));
    }

    #[test]
    fn parse_apply_patch_strips_heredoc_double_quoted() {
        let patch = "\
<<\"EOF\"
*** Begin Patch
*** Delete File: src/old.rs
*** End Patch
EOF";

        let ops = parse_apply_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], PatchOperation::Delete { .. }));
    }

    // ---- End-to-end tests: raw patch text → parse → apply → verify ----

    #[tokio::test]
    async fn e2e_canonical_format_multi_hunk_update() {
        // Realistic Python file with canonical @@ format (no trailing @@)
        let mut files = HashMap::new();
        files.insert(
            "src/game.py".to_string(),
            "\
from dataclasses import dataclass, field
from src.cards import Suit
import random

@dataclass
class GameState:
    stock: list = field(default_factory=list)
    waste: list = field(default_factory=list)
    tableau: list = field(default_factory=list)

    def deal(self):
        random.shuffle(self.stock)
        for i in range(7):
            self.tableau.append(self.stock.pop())

    def draw(self):
        if self.stock:
            self.waste.append(self.stock.pop())
"
            .to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let patch = "\
*** Begin Patch
*** Update File: src/game.py
@@ from dataclasses import dataclass, field
-from src.cards import Suit
+from src.cards import Card, Suit
@@ class GameState:
-    stock: list = field(default_factory=list)
-    waste: list = field(default_factory=list)
-    tableau: list = field(default_factory=list)
+    stock: list[Card] = field(default_factory=list)
+    waste: list[Card] = field(default_factory=list)
+    tableau: list[Card] = field(default_factory=list)
@@ def draw(self):
-        if self.stock:
-            self.waste.append(self.stock.pop())
+        card = self.stock.pop() if self.stock else None
+        if card:
+            self.waste.append(card)
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        apply_patch_operations(&ops, &env).await.unwrap();

        let content = env.read_file_text("src/game.py").await.unwrap();
        assert!(content.contains("from src.cards import Card, Suit"));
        assert!(content.contains("stock: list[Card]"));
        assert!(content.contains("waste: list[Card]"));
        assert!(content.contains("tableau: list[Card]"));
        assert!(content.contains("card = self.stock.pop()"));
        assert!(content.contains("self.waste.append(card)"));
        // Untouched lines preserved
        assert!(content.contains("from dataclasses import dataclass, field"));
        assert!(content.contains("import random"));
        assert!(content.contains("def deal(self):"));
        assert!(content.contains("random.shuffle(self.stock)"));
    }

    #[tokio::test]
    async fn e2e_multi_operation_add_update_delete() {
        let mut files = HashMap::new();
        files.insert(
            "src/old_util.py".to_string(),
            "def old_helper():\n    pass\n".to_string(),
        );
        files.insert(
            "src/main.py".to_string(),
            "\
from old_util import old_helper

def main():
    old_helper()
    print(\"done\")
"
            .to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let patch = "\
*** Begin Patch
*** Add File: src/new_util.py
+def new_helper():
+    return 42
*** Delete File: src/old_util.py
*** Update File: src/main.py
@@
-from old_util import old_helper
+from new_util import new_helper
@@ def main():
-    old_helper()
+    result = new_helper()
+    print(f\"result: {result}\")
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        let result = apply_patch_operations(&ops, &env).await.unwrap();

        assert!(result.contains("A src/new_util.py"));
        assert!(result.contains("D src/old_util.py"));
        assert!(result.contains("M src/main.py"));

        let new_util = env.read_file_text("src/new_util.py").await.unwrap();
        assert_eq!(new_util, "def new_helper():\n    return 42\n");

        assert!(env.read_file_text("src/old_util.py").await.is_err());

        let main = env.read_file_text("src/main.py").await.unwrap();
        assert!(main.contains("from new_util import new_helper"));
        assert!(main.contains("result = new_helper()"));
        assert!(main.contains("print(\"done\")"));
    }

    #[tokio::test]
    async fn e2e_heredoc_stacked_context_end_of_file_and_move() {
        let mut files = HashMap::new();
        files.insert(
            "src/models/user.py".to_string(),
            "\
class User:
    def __init__(self, name):
        self.name = name
        self.active = True

    def greet(self):
        return f\"Hello, {self.name}\"

    def deactivate(self):
        self.active = False
"
            .to_string(),
        );
        let env = MutableMockSandbox::new(files);

        // Heredoc-wrapped patch with stacked @@, End of File, and Move to
        let patch = "\
<<'EOF'
*** Begin Patch
*** Update File: src/models/user.py
*** Move to: src/models/account.py
@@ class User:
@@     def __init__(self, name):
-        self.name = name
-        self.active = True
+        self.name = name
+        self.email = None
+        self.active = True
@@ class User:
@@     def deactivate(self):
-        self.active = False
+        self.active = False
+        self.email = None
*** End of File
*** End Patch
EOF";

        let ops = parse_apply_patch(patch).unwrap();
        let result = apply_patch_operations(&ops, &env).await.unwrap();

        assert!(result.contains("M src/models/account.py"));

        // Old path gone
        assert!(env.read_file_text("src/models/user.py").await.is_err());

        // New path has updated content
        let content = env.read_file_text("src/models/account.py").await.unwrap();
        assert!(content.contains("self.email = None"));
        assert!(content.contains("self.active = True"));
        assert!(content.contains("def greet(self):"));
        // The End of File hunk should have matched the LAST deactivate method
        let deactivate_pos = content.rfind("def deactivate").unwrap();
        let after_deactivate = &content[deactivate_pos..];
        assert!(after_deactivate.contains("self.email = None"));
    }

    #[tokio::test]
    async fn e2e_forward_cursor_with_duplicate_patterns() {
        // File has three identical "pass" lines; three bare @@ hunks should
        // replace them in order thanks to forward cursor
        let mut files = HashMap::new();
        files.insert(
            "src/stubs.py".to_string(),
            "\
def alpha():
    pass

def beta():
    pass

def gamma():
    pass
"
            .to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let patch = "\
*** Begin Patch
*** Update File: src/stubs.py
@@ def alpha():
-    pass
+    return \"a\"
@@ def beta():
-    pass
+    return \"b\"
@@ def gamma():
-    pass
+    return \"c\"
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        apply_patch_operations(&ops, &env).await.unwrap();

        let content = env.read_file_text("src/stubs.py").await.unwrap();
        assert!(content.contains("return \"a\""));
        assert!(content.contains("return \"b\""));
        assert!(content.contains("return \"c\""));
        assert!(!content.contains("    pass"));
    }

    #[tokio::test]
    async fn e2e_fuzzy_matching_with_trailing_whitespace() {
        // File has trailing whitespace on lines; patch doesn't
        let mut files = HashMap::new();
        files.insert(
            "src/lib.rs".to_string(),
            "fn main() {  \n    println!(\"hello\");  \n}\n".to_string(),
        );
        let env = MutableMockSandbox::new(files);

        let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
@@ fn main() {
-    println!(\"hello\");
+    println!(\"world\");
*** End Patch";

        let ops = parse_apply_patch(patch).unwrap();
        apply_patch_operations(&ops, &env).await.unwrap();

        let content = env.read_file_text("src/lib.rs").await.unwrap();
        assert!(content.contains("println!(\"world\")"));
        assert!(!content.contains("println!(\"hello\")"));
    }

    #[tokio::test]
    async fn e2e_through_tool_executor() {
        use crate::config::SessionOptions;
        use crate::session::Session;
        use crate::test_support::{MockLlmProvider, TestProfile, make_client, text_response};
        use crate::tool_registry::ToolRegistry;

        // Set up sandbox with a file
        let mut files = HashMap::new();
        files.insert(
            "src/app.py".to_string(),
            "\
def greet(name):
    return f\"Hi, {name}\"

def farewell(name):
    return f\"Bye, {name}\"
"
            .to_string(),
        );
        files.insert(
            "src/obsolete.py".to_string(),
            "def old():\n    pass\n".to_string(),
        );
        let env = Arc::new(MutableMockSandbox::new(files));

        // Register apply_patch tool
        let mut registry = ToolRegistry::new();
        registry.register(make_apply_patch_tool());

        // The patch an LLM would produce (canonical format)
        let patch_text = "\
*** Begin Patch
*** Add File: src/created.py
+def created():
+    return \"created\"
*** Update File: src/app.py
@@ def greet(name):
-    return f\"Hi, {name}\"
+    return f\"Hello, {name}!\"
@@ def farewell(name):
-    return f\"Bye, {name}\"
+    return f\"Goodbye, {name}!\"
*** Delete File: src/obsolete.py
*** End Patch";

        let mut tool_call = ToolCall::new("call_1", "apply_patch", serde_json::json!(patch_text));
        tool_call.tool_type = "custom".to_string();
        tool_call.raw_arguments = Some(patch_text.to_string());
        let responses = vec![
            Response {
                id:            "resp_call_1".to_string(),
                model:         "mock-model".to_string(),
                provider:      "mock".to_string(),
                message:       LlmMessage {
                    role:         Role::Assistant,
                    content:      vec![ContentPart::ToolCall(tool_call)],
                    name:         None,
                    tool_call_id: None,
                },
                finish_reason: FinishReason::ToolCalls,
                usage:         TokenCounts::default(),
                raw:           None,
                warnings:      vec![],
                rate_limit:    None,
                cost_usd:      None,
                cost_source:   None,
            },
            text_response("Done! Updated greet and farewell functions."),
        ];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::with_tools(registry));
        let mut session = Session::new(
            client,
            profile,
            env.clone(),
            SessionOptions::default(),
            None,
        );
        session.initialize().await.unwrap();
        session
            .process_input("Update the greeting functions")
            .await
            .unwrap();

        let content = env.read_file_text("src/app.py").await.unwrap();
        assert!(content.contains("Hello, {name}!"));
        assert!(content.contains("Goodbye, {name}!"));
        assert!(!content.contains("Hi, {name}"));
        assert!(!content.contains("Bye, {name}"));

        let created = env.read_file_text("src/created.py").await.unwrap();
        assert!(created.contains("def created():"));
        assert!(env.read_file_text("src/obsolete.py").await.is_err());
    }

    #[tokio::test]
    async fn failed_custom_tool_call_returns_codex_style_error_to_session_history() {
        use crate::config::SessionOptions;
        use crate::session::Session;
        use crate::test_support::{MockLlmProvider, TestProfile, make_client, text_response};
        use crate::tool_registry::ToolRegistry;
        use crate::types::Message as AgentMessage;

        let mut files = HashMap::new();
        files.insert(
            "src/app.py".to_string(),
            "def present():\n    return 1\n".to_string(),
        );
        let env = Arc::new(MutableMockSandbox::new(files));

        let mut registry = ToolRegistry::new();
        registry.register(make_apply_patch_tool());

        let patch_text = "\
*** Begin Patch
*** Update File: src/app.py
@@ def missing():
-    return 1
+    return 2
*** End Patch";
        let mut tool_call = ToolCall::new("call_1", "apply_patch", serde_json::json!(patch_text));
        tool_call.tool_type = "custom".to_string();
        tool_call.raw_arguments = Some(patch_text.to_string());

        let responses = vec![
            Response {
                id:            "resp_call_1".to_string(),
                model:         "mock-model".to_string(),
                provider:      "mock".to_string(),
                message:       LlmMessage {
                    role:         Role::Assistant,
                    content:      vec![ContentPart::ToolCall(tool_call)],
                    name:         None,
                    tool_call_id: None,
                },
                finish_reason: FinishReason::ToolCalls,
                usage:         TokenCounts::default(),
                raw:           None,
                warnings:      vec![],
                rate_limit:    None,
                cost_usd:      None,
                cost_source:   None,
            },
            text_response("I will correct the patch."),
        ];

        let provider = Arc::new(MockLlmProvider::new(responses));
        let client = make_client(provider).await;
        let profile = Arc::new(TestProfile::with_tools(registry));
        let mut session = Session::new(client, profile, env, SessionOptions::default(), None);
        session.initialize().await.unwrap();
        session
            .process_input("Patch a missing function")
            .await
            .unwrap();

        let turns = session.history().turns();
        match &turns[2] {
            AgentMessage::ToolResults { results, .. } => {
                assert_eq!(results.len(), 1);
                assert!(results[0].is_error);
                assert_eq!(
                    results[0].content.as_str(),
                    Some("Failed to find context 'def missing():' in src/app.py")
                );
            }
            other => panic!("expected tool result turn, got {other:?}"),
        }
    }
}
