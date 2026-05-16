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
    // Public module declaration for the bootstrap hatch.
    "lib/crates/fabro-model/src/lib.rs",
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

/// Production runtime code should build catalogs from resolved settings and
/// thread the resulting `Arc<Catalog>` through state. Direct use of
/// `Catalog::builtin()` is reserved for `fabro-model` internals and tests.
const CATALOG_BUILTIN_ALLOWED_PATH_FRAGMENTS: &[&str] = &[
    // The catalog owner may define and test the built-in/default catalog.
    "lib/crates/fabro-model/",
    // Tests and test support may use built-ins as fixtures.
    "/tests/",
    "/tests/it/",
    "test_support",
    "/tests/policy.rs",
];

const TEMPLATE_RENDER_ALLOWED_PATH_FRAGMENTS: &[&str] = &[
    // The template crate owns the rendering API and its tests.
    "lib/crates/fabro-template/src/lib.rs",
    // Workflow-definition rendering must stay centralized here.
    "lib/crates/fabro-workflow/src/transforms/variable_expansion.rs",
    // Hook header/env interpolation is a separate system.
    "lib/crates/fabro-hooks/src/executor.rs",
    // This policy test names the forbidden patterns.
    "/tests/it/policy.rs",
];

const TEMPLATE_RENDER_FORBIDDEN_PATTERNS: &[&str] = &[
    "render_template(",
    "render_lenient(",
    "render_scan_template",
    "render as render_template",
    "render_lenient as",
    "fabro_template::{",
];

#[test]
fn bootstrap_catalog_references_stay_in_allowlist() {
    let violations = source_symbol_violations(
        "bootstrap_catalog",
        BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS,
    );

    assert!(
        violations.is_empty(),
        "bootstrap_catalog (install-only) referenced from non-allowlisted source files:\n{}\n\nIf this is intentional, add the path fragment to BOOTSTRAP_CATALOG_ALLOWED_PATH_FRAGMENTS in lib/crates/fabro-dev/tests/it/policy.rs.",
        format_violations(violations),
    );
}

#[test]
fn catalog_builtin_references_stay_in_allowlist() {
    let violations =
        source_symbol_violations("Catalog::builtin()", CATALOG_BUILTIN_ALLOWED_PATH_FRAGMENTS);

    assert!(
        violations.is_empty(),
        "Catalog::builtin() referenced from non-allowlisted production source files:\n{}\n\nRuntime code should use a resolved settings catalog via `Catalog::from_builtin_with_overrides(...)` or an injected `Arc<Catalog>`. If this is intentional test/bootstrap code, add the path fragment to CATALOG_BUILTIN_ALLOWED_PATH_FRAGMENTS in lib/crates/fabro-dev/tests/it/policy.rs.",
        format_violations(violations),
    );
}

#[test]
fn workflow_template_rendering_call_sites_stay_in_allowlist() {
    let mut violations = Vec::new();
    for pattern in TEMPLATE_RENDER_FORBIDDEN_PATTERNS {
        violations.extend(source_symbol_violations(
            pattern,
            TEMPLATE_RENDER_ALLOWED_PATH_FRAGMENTS,
        ));
    }

    assert!(
        violations.is_empty(),
        "Workflow template rendering must go through TemplateTransform. Add an allowlist entry only for non-workflow interpolation with a reason:\n{}",
        format_violations(violations),
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "policy test reads source files synchronously with std::fs"
)]
fn source_symbol_violations(
    symbol: &str,
    allowed_path_fragments: &[&str],
) -> Vec<(String, usize, String)> {
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
        if !contents.contains(symbol) {
            continue;
        }
        let rel = path.strip_prefix(&root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let path_allowed = allowed_path_fragments
            .iter()
            .any(|frag| rel_str.contains(frag));
        if path_allowed {
            continue;
        }
        let mut pending_cfg_test = false;
        let mut cfg_test_depth = None;
        let mut brace_depth = 0usize;
        for (idx, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            let starts_cfg_test_module =
                pending_cfg_test && trimmed.contains("mod tests") && trimmed.contains('{');
            let in_cfg_test_module = cfg_test_depth.is_some() || starts_cfg_test_module;

            if !line.contains(symbol) {
                update_test_module_state(
                    trimmed,
                    &mut pending_cfg_test,
                    &mut cfg_test_depth,
                    &mut brace_depth,
                    starts_cfg_test_module,
                );
                continue;
            }
            // Skip comments referencing the symbol in prose.
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                update_test_module_state(
                    trimmed,
                    &mut pending_cfg_test,
                    &mut cfg_test_depth,
                    &mut brace_depth,
                    starts_cfg_test_module,
                );
                continue;
            }
            if in_cfg_test_module {
                update_test_module_state(
                    trimmed,
                    &mut pending_cfg_test,
                    &mut cfg_test_depth,
                    &mut brace_depth,
                    starts_cfg_test_module,
                );
                continue;
            }
            violations.push((rel_str.clone(), idx + 1, line.to_string()));
            update_test_module_state(
                trimmed,
                &mut pending_cfg_test,
                &mut cfg_test_depth,
                &mut brace_depth,
                starts_cfg_test_module,
            );
        }
    }

    violations
}

fn update_test_module_state(
    trimmed: &str,
    pending_cfg_test: &mut bool,
    cfg_test_depth: &mut Option<usize>,
    brace_depth: &mut usize,
    starts_cfg_test_module: bool,
) {
    let depth_before = *brace_depth;
    let open_count = trimmed.chars().filter(|c| *c == '{').count();
    let close_count = trimmed.chars().filter(|c| *c == '}').count();
    *brace_depth = brace_depth.saturating_add(open_count);
    *brace_depth = brace_depth.saturating_sub(close_count);

    if starts_cfg_test_module {
        *cfg_test_depth = Some(
            depth_before
                .saturating_add(open_count)
                .saturating_sub(close_count),
        );
    }
    if cfg_test_depth.is_some_and(|depth| *brace_depth < depth) {
        *cfg_test_depth = None;
    }

    if trimmed.starts_with("#[cfg(test)]") {
        *pending_cfg_test = true;
    } else if !trimmed.is_empty() && !trimmed.starts_with("#[") {
        *pending_cfg_test = false;
    }
}

fn format_violations(violations: Vec<(String, usize, String)>) -> String {
    violations
        .into_iter()
        .map(|(p, l, s)| format!("  {p}:{l}: {}", s.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}
