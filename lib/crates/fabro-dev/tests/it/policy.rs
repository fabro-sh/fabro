//! Workspace policy tests.
//!
//! These tests scan the source tree for references that violate
//! product-level invariants. They run as part of `cargo nextest` and are
//! cheap (text scans only).

use walkdir::WalkDir;

use crate::workspace_root;

/// `fabro_model::bootstrap_catalog` (and its module) is the install/API-key
/// validation hatch from the settings-driven LLM catalog plan. It must
/// **not** appear in request-serving paths — server handlers, workflow
/// operations, agent runtime, hooks, or completion handlers — because those
/// must use the resolved `Arc<Catalog>` threaded through their state.
///
/// The allowed-callers list below is the policy boundary. Adding a new
/// caller is intentional and requires updating this list.
///
/// The walker only descends into `lib/`, so non-`lib/` paths (docs, top-level
/// markdown) are not part of the allowlist.
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
];

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "policy test reads source files synchronously with std::fs"
)]
fn bootstrap_catalog_references_stay_in_allowlist() {
    let root = workspace_root();
    let lib_root = root.join("lib");
    let mut violations: Vec<(String, usize, String)> = Vec::new();

    let walker = WalkDir::new(&lib_root).into_iter().filter_entry(|entry| {
        // Skip generated/output directories at any depth.
        let name = entry.file_name().to_string_lossy();
        !matches!(
            name.as_ref(),
            "target" | ".git" | "node_modules" | "dist" | "build"
        )
    });

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        // Cheap early-out: avoids per-line work for the ~99% of files with no
        // reference to the symbol.
        if !contents.contains("bootstrap_catalog") {
            continue;
        }
        let rel = path.strip_prefix(&root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let path_allowed = BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS
            .iter()
            .any(|frag| rel_str.contains(frag));
        if path_allowed {
            continue;
        }
        for (idx, line) in contents.lines().enumerate() {
            if !line.contains("bootstrap_catalog") {
                continue;
            }
            // Skip comments referencing the symbol in prose.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }
            violations.push((rel_str.clone(), idx + 1, line.to_string()));
        }
    }

    assert!(
        violations.is_empty(),
        "bootstrap_catalog (install-only) referenced from non-allowlisted source files:\n{}\n\nIf this is intentional, add the path fragment to BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS in lib/crates/fabro-dev/tests/it/policy.rs.",
        violations
            .into_iter()
            .map(|(p, l, s)| format!("  {p}:{l}: {}", s.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
