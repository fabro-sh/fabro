#![expect(
    clippy::disallowed_methods,
    reason = "sync workflow file resolver invoked at stage setup; not on a Tokio hot path"
)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub trait FileResolver: Send + Sync {
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFile {
    pub logical_path: PathBuf,
    pub content:      String,
}

#[derive(Clone, Debug, Default)]
pub struct BundleFileResolver {
    files: HashMap<PathBuf, String>,
}

impl BundleFileResolver {
    #[must_use]
    pub fn new(files: HashMap<PathBuf, String>) -> Self {
        Self { files }
    }
}

impl FileResolver for BundleFileResolver {
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile> {
        let logical_path = normalize_logical_path(current_dir, reference)?;
        self.files.get(&logical_path).map(|content| ResolvedFile {
            logical_path,
            content: content.clone(),
        })
    }
}

pub(crate) fn normalize_logical_path(current_dir: &Path, reference: &str) -> Option<PathBuf> {
    let path = Path::new(reference);
    if path.is_absolute() || reference.starts_with('~') {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in current_dir.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
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
                logical_path: resolved_path,
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

    #[test]
    fn bundle_resolver_returns_exact_match() {
        let resolver = BundleFileResolver::new(HashMap::from([(
            PathBuf::from("prompts/review.md"),
            "check it".to_string(),
        )]));

        let resolved = resolver
            .resolve(Path::new("."), "prompts/review.md")
            .expect("file should resolve");

        assert_eq!(resolved.logical_path, PathBuf::from("prompts/review.md"));
        assert_eq!(resolved.content, "check it");
    }

    #[test]
    fn bundle_resolver_normalizes_relative_segments() {
        let resolver = BundleFileResolver::new(HashMap::from([(
            PathBuf::from("prompts/review.md"),
            "check it".to_string(),
        )]));

        let resolved = resolver
            .resolve(Path::new("subflows"), "../prompts/review.md")
            .expect("file should resolve");

        assert_eq!(resolved.logical_path, PathBuf::from("prompts/review.md"));
    }

    #[test]
    fn bundle_resolver_returns_none_for_missing_path() {
        let resolver = BundleFileResolver::new(HashMap::new());
        assert!(resolver.resolve(Path::new("."), "missing.md").is_none());
    }
}
