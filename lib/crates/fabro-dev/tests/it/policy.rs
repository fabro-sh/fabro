//! Workspace policy tests.
//!
//! These tests scan the source tree for references that violate
//! product-level invariants. They run as part of `cargo nextest` and are
//! cheap (text scans only).

use std::path::{Path, PathBuf};

use crate::workspace_root;

/// `fabro_model::bootstrap_catalog` (and its module) is the install/API-key
/// validation hatch from the settings-driven LLM catalog plan. It must
/// **not** appear in request-serving paths — server handlers, workflow
/// operations, agent runtime, hooks, or completion handlers — because those
/// must use the resolved `Arc<Catalog>` threaded through their state.
///
/// The allowed-callers list below is the policy boundary. Adding a new
/// caller is intentional and requires updating this list.
const BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS: &[&str] = &[
    // The bootstrap module itself.
    "lib/crates/fabro-model/src/bootstrap_catalog",
    // Install / first-run / API-key validation flows that legitimately need
    // a built-in catalog before any project settings have been loaded.
    "lib/crates/fabro-install/",
    "lib/crates/fabro-cli/src/commands/install/",
    "lib/crates/fabro-cli/src/shared/install_",
    "lib/crates/fabro-cli/src/shared/api_key_validation",
    // Test support modules.
    "tests/",
    "test_support",
    "/tests/it/",
    "/tests/policy.rs",
    // Documentation files referencing the policy.
    "docs/",
    "CLAUDE.md",
    "AGENTS.md",
];

#[test]
fn bootstrap_catalog_references_stay_in_allowlist() {
    let root = workspace_root();
    let mut violations: Vec<(PathBuf, usize, String)> = Vec::new();
    walk_rust_sources(&root, &mut |path, contents| {
        for (idx, line) in contents.lines().enumerate() {
            if !line.contains("bootstrap_catalog") {
                continue;
            }
            // Skip comments referencing the symbol in prose.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }
            let rel = path.strip_prefix(&root).unwrap_or(path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS
                .iter()
                .any(|frag| rel_str.contains(frag))
            {
                continue;
            }
            violations.push((rel.to_path_buf(), idx + 1, line.to_string()));
        }
    });

    assert!(
        violations.is_empty(),
        "bootstrap_catalog (install-only) referenced from non-allowlisted source files:\n{}\n\nIf this is intentional, add the path fragment to BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS in lib/crates/fabro-dev/tests/it/policy.rs.",
        violations
            .into_iter()
            .map(|(p, l, s)| format!("  {}:{}: {}", p.display(), l, s.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "policy test reads source files synchronously with std::fs"
)]
fn walk_rust_sources(root: &Path, on_file: &mut dyn FnMut(&Path, &str)) {
    let mut stack: Vec<PathBuf> = vec![root.join("lib")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Skip generated/output directories.
            if matches!(
                name_str.as_ref(),
                "target" | ".git" | "node_modules" | "dist" | "build"
            ) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    on_file(&path, &contents);
                }
            }
        }
    }
}
