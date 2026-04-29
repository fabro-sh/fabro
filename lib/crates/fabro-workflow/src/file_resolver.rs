#![expect(
    clippy::disallowed_methods,
    reason = "sync workflow file resolver invoked at stage setup; not on a Tokio hot path"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ManifestPath;

pub trait FileResolver: Send + Sync {
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFile {
    pub path:    PathBuf,
    pub content: String,
}

#[derive(Clone, Debug, Default)]
pub struct BundleFileResolver {
    files: HashMap<ManifestPath, String>,
}

impl BundleFileResolver {
    #[must_use]
    pub fn new(files: HashMap<ManifestPath, String>) -> Self {
        Self { files }
    }
}

impl FileResolver for BundleFileResolver {
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile> {
        let path = ManifestPath::from_reference(current_dir, reference)?;
        let content = self.files.get(&path)?.clone();
        Some(ResolvedFile {
            path: path.into(),
            content,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct FilesystemFileResolver {
    fallback_dir: Option<PathBuf>,
}

impl FilesystemFileResolver {
    #[must_use]
    pub fn new(fallback_dir: Option<PathBuf>) -> Self {
        Self { fallback_dir }
    }
}

impl FileResolver for FilesystemFileResolver {
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile> {
        let raw = Path::new(reference);
        let is_tilde = reference.starts_with('~');
        let expanded = if is_tilde {
            match dirs::home_dir() {
                Some(home) => home.join(raw.strip_prefix("~").unwrap_or_else(|_| Path::new(""))),
                None => current_dir.join(reference),
            }
        } else {
            current_dir.join(reference)
        };

        let resolved_path = match expanded.canonicalize() {
            Ok(path) if path.is_file() => Some(path),
            _ if !is_tilde => self.fallback_dir.as_ref().and_then(|fallback_dir| {
                let fallback_path = fallback_dir.join(reference);
                match fallback_path.canonicalize() {
                    Ok(path) if path.is_file() => Some(path),
                    _ => None,
                }
            }),
            _ => None,
        }?;

        match std::fs::read_to_string(&resolved_path) {
            Ok(content) => Some(ResolvedFile {
                path: resolved_path,
                content,
            }),
            Err(error) => {
                tracing::warn!(
                    path = %resolved_path.display(),
                    %error,
                    "Failed to read file reference"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("path should parse")
    }

    #[test]
    fn bundle_resolver_returns_exact_match() {
        let resolver = BundleFileResolver::new(HashMap::from([(
            manifest_path("prompts/review.md"),
            "check it".to_string(),
        )]));

        let resolved = resolver
            .resolve(Path::new("."), "prompts/review.md")
            .expect("file should resolve");

        assert_eq!(resolved.path, PathBuf::from("prompts/review.md"));
        assert_eq!(resolved.content, "check it");
    }

    #[test]
    fn bundle_resolver_normalizes_relative_segments() {
        let resolver = BundleFileResolver::new(HashMap::from([(
            manifest_path("prompts/review.md"),
            "check it".to_string(),
        )]));

        let resolved = resolver
            .resolve(Path::new("subflows"), "../prompts/review.md")
            .expect("file should resolve");

        assert_eq!(resolved.path, PathBuf::from("prompts/review.md"));
    }

    #[test]
    fn bundle_resolver_returns_none_for_missing_path() {
        let resolver = BundleFileResolver::new(HashMap::new());
        assert!(resolver.resolve(Path::new("."), "missing.md").is_none());
    }

    #[test]
    fn bundle_resolver_resolves_outside_cwd_paths() {
        let resolver = BundleFileResolver::new(HashMap::from([(
            manifest_path("../.fabro/workflows/demo/prompts/hello.md"),
            "prompt content".to_string(),
        )]));

        let resolved = resolver
            .resolve(Path::new("../.fabro/workflows/demo"), "prompts/hello.md")
            .expect("file should resolve for out-of-CWD workflow");

        assert_eq!(resolved.content, "prompt content");
    }
}
