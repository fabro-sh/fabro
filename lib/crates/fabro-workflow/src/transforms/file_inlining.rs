use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_graphviz::graph::{AttrValue, Graph};

use crate::file_resolver::FileResolver;

use super::Transform;

/// Resolve a potential `@path` file reference.
///
/// If `value` starts with `@` and the referenced file exists locally, the file
/// contents are returned (inlined). Otherwise the original value is returned
/// unchanged.
pub fn resolve_file_ref(value: &str, current_dir: &Path, resolver: &dyn FileResolver) -> String {
    let Some(path_str) = value.strip_prefix('@') else {
        return value.to_string();
    };
    resolver
        .resolve(current_dir, path_str)
        .map_or_else(|| value.to_string(), |resolved| resolved.content)
}

/// Inlines `@file` references in node prompts and the graph-level goal.
pub struct FileInliningTransform {
    current_dir: PathBuf,
    resolver: Arc<dyn FileResolver>,
}

impl FileInliningTransform {
    #[must_use]
    pub fn new(current_dir: PathBuf, resolver: Arc<dyn FileResolver>) -> Self {
        Self {
            current_dir,
            resolver,
        }
    }
}

impl Transform for FileInliningTransform {
    fn apply(&self, graph: Graph) -> Graph {
        let mut graph = graph;

        // Inline @file refs in node prompts
        for node in graph.nodes.values_mut() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                let resolved = resolve_file_ref(prompt, &self.current_dir, self.resolver.as_ref());
                if resolved != *prompt {
                    node.attrs
                        .insert("prompt".to_string(), AttrValue::String(resolved));
                }
            }
        }

        // Inline @file refs in graph-level goal
        if let Some(AttrValue::String(goal)) = graph.attrs.get("goal") {
            let resolved = resolve_file_ref(goal, &self.current_dir, self.resolver.as_ref());
            if resolved != *goal {
                graph
                    .attrs
                    .insert("goal".to_string(), AttrValue::String(resolved));
            }
        }

        graph
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fabro_graphviz::graph::{AttrValue, Graph, Node};

    use super::*;
    use crate::file_resolver::FilesystemFileResolver;

    #[test]
    fn resolve_file_ref_passthrough_non_at() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref(
                "hello world",
                dir.path(),
                &FilesystemFileResolver::new(None),
            ),
            "hello world"
        );
    }

    #[test]
    fn resolve_file_ref_passthrough_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref(
                "@nonexistent.md",
                dir.path(),
                &FilesystemFileResolver::new(None),
            ),
            "@nonexistent.md"
        );
    }

    #[test]
    fn resolve_file_ref_inlines_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.md"), "inlined content").unwrap();

        assert_eq!(
            resolve_file_ref("@prompt.md", dir.path(), &FilesystemFileResolver::new(None)),
            "inlined content"
        );
    }

    #[test]
    fn file_inlining_transform_inlines_prompt_and_goal() {
        let dir = tempfile::tempdir().unwrap();
        // Init repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join("prompt.md"), "Do the work").unwrap();
        std::fs::write(dir.path().join("goal.md"), "Ship feature").unwrap();

        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("@goal.md".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@prompt.md".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let transform = FileInliningTransform::new(
            dir.path().to_path_buf(),
            Arc::new(FilesystemFileResolver::new(None)),
        );
        let graph = transform.apply(graph);

        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Do the work")
        );
        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("Ship feature")
        );
    }

    #[test]
    fn resolve_file_ref_expands_tilde() {
        let home = dirs::home_dir().expect("home dir must exist");
        let test_file = home.join(".fabro_test_tilde_tmp");
        std::fs::write(&test_file, "tilde content").unwrap();
        let _cleanup = scopeguard::guard((), |()| {
            let _ = std::fs::remove_file(&test_file);
        });

        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            resolve_file_ref(
                "@~/.fabro_test_tilde_tmp",
                dir.path(),
                &FilesystemFileResolver::new(None),
            ),
            "tilde content"
        );
    }

    #[test]
    fn resolve_file_ref_resolves_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.md"), "dotdot content").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        assert_eq!(
            resolve_file_ref(
                "@subdir/../file.md",
                dir.path(),
                &FilesystemFileResolver::new(None),
            ),
            "dotdot content"
        );
    }

    #[test]
    fn resolve_file_ref_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared content").unwrap();

        assert_eq!(
            resolve_file_ref(
                "@shared.md",
                base.path(),
                &FilesystemFileResolver::new(Some(fallback.path().to_path_buf())),
            ),
            "shared content"
        );
    }

    #[test]
    fn resolve_file_ref_base_dir_takes_precedence_over_fallback() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("prompt.md"), "base content").unwrap();
        std::fs::write(fallback.path().join("prompt.md"), "fallback content").unwrap();

        assert_eq!(
            resolve_file_ref(
                "@prompt.md",
                base.path(),
                &FilesystemFileResolver::new(Some(fallback.path().to_path_buf())),
            ),
            "base content"
        );
    }

    #[test]
    fn resolve_file_ref_no_fallback_for_tilde_path() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("file.md"), "fallback").unwrap();

        // Tilde path to nonexistent file should return original value, not try fallback
        let result = resolve_file_ref(
            "@~/nonexistent_fabro_test.md",
            base.path(),
            &FilesystemFileResolver::new(Some(fallback.path().to_path_buf())),
        );
        assert_eq!(result, "@~/nonexistent_fabro_test.md");
    }

    #[test]
    fn resolve_file_ref_fallback_none_behaves_as_before() {
        let base = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref(
                "@missing.md",
                base.path(),
                &FilesystemFileResolver::new(None)
            ),
            "@missing.md"
        );
    }

    #[test]
    fn file_inlining_transform_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared prompt").unwrap();

        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@shared.md".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let transform = FileInliningTransform::new(
            base.path().to_path_buf(),
            Arc::new(FilesystemFileResolver::new(Some(
                fallback.path().to_path_buf(),
            ))),
        );
        let graph = transform.apply(graph);

        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("shared prompt")
        );
    }
}
