use std::fs;
use std::path::PathBuf;

use walkdir::WalkDir;

const ALLOWLIST: &[(&str, &str)] = &[
    (
        "lib/crates/fabro-workflow/src/transforms/variable_expansion.rs",
        "the only workflow-definition renderer",
    ),
    (
        "lib/crates/fabro-hooks/src/executor.rs",
        "hook header/env interpolation is a separate system",
    ),
];

const FORBIDDEN_PATTERNS: &[&str] = &[
    "render_template(",
    "render_lenient(",
    "render_scan_template",
    "render as render_template",
    "render_lenient as",
    "fabro_template::{",
];

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "guardrail test intentionally scans Rust source files synchronously"
)]
fn workflow_template_rendering_call_sites_are_allowlisted() {
    let workspace = workspace_root();
    let crates = workspace.join("lib/crates");
    let mut violations = Vec::new();

    for entry in WalkDir::new(&crates).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let relative = path.strip_prefix(&workspace).unwrap_or(path);
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative == "lib/crates/fabro-template/src/lib.rs"
            || relative == "lib/crates/fabro-workflow/tests/template_render_call_sites.rs"
            || is_allowlisted(&relative)
        {
            continue;
        }

        let source = fs::read_to_string(path).expect("Rust source should be readable");
        if FORBIDDEN_PATTERNS
            .iter()
            .any(|pattern| source.contains(pattern))
        {
            violations.push(relative);
        }
    }

    assert!(
        violations.is_empty(),
        "Workflow template rendering must go through TemplateTransform. Add an allowlist entry only for non-workflow interpolation with a reason. Violating files: {}",
        violations.join(", ")
    );
}

fn workspace_root() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while path.file_name().and_then(|name| name.to_str()) != Some("lib") {
        assert!(
            path.pop(),
            "could not find workspace root from CARGO_MANIFEST_DIR"
        );
    }
    path.parent()
        .expect("lib directory should have a parent")
        .to_path_buf()
}

fn is_allowlisted(relative: &str) -> bool {
    ALLOWLIST.iter().any(|(path, _reason)| relative == *path)
}
