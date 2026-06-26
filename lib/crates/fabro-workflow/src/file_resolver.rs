#![expect(
    clippy::disallowed_methods,
    reason = "sync workflow file resolver invoked at stage setup; not on a Tokio hot path"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_template::{TemplateIncludeResolver, TemplateLoadError, TemplateSource, TemplateStore};
use fabro_types::ManifestPath;

pub trait FileResolver: Send + Sync {
    fn resolve(&self, current_dir: &Path, reference: &str) -> Option<ResolvedFile>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFile {
    pub path:    PathBuf,
    pub content: String,
}

#[derive(Clone)]
pub struct FileResolverTemplateStore {
    base_dir: PathBuf,
    resolver: Arc<dyn FileResolver>,
}

impl FileResolverTemplateStore {
    #[must_use]
    pub fn new(base_dir: PathBuf, resolver: Arc<dyn FileResolver>) -> Self {
        Self { base_dir, resolver }
    }
}

impl TemplateStore for FileResolverTemplateStore {
    fn load(
        &self,
        parent: &TemplateSource,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let path =
            TemplateIncludeResolver::new(parent.root.clone()).resolve(&parent.path, reference)?;
        Ok(self
            .resolver
            .resolve(&self.base_dir, &path.to_string())
            .map(|resolved| TemplateSource::new(path, parent.root.clone(), resolved.content)))
    }
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

    #[test]
    fn filesystem_resolver_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.md"), "inlined content").unwrap();

        let resolved = FilesystemFileResolver::new(None)
            .resolve(dir.path(), "prompt.md")
            .expect("file should resolve");

        assert_eq!(resolved.content, "inlined content");
    }

    #[test]
    fn filesystem_resolver_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        assert!(
            FilesystemFileResolver::new(None)
                .resolve(dir.path(), "nonexistent.md")
                .is_none()
        );
    }

    #[test]
    fn filesystem_resolver_expands_tilde() {
        let home = dirs::home_dir().expect("home dir must exist");
        let test_file = home.join(".fabro_test_tilde_tmp");
        std::fs::write(&test_file, "tilde content").unwrap();
        let _cleanup = scopeguard::guard((), |()| {
            let _ = std::fs::remove_file(&test_file);
        });

        let dir = tempfile::tempdir().unwrap();
        let resolved = FilesystemFileResolver::new(None)
            .resolve(dir.path(), "~/.fabro_test_tilde_tmp")
            .expect("tilde path should resolve");

        assert_eq!(resolved.content, "tilde content");
    }

    #[test]
    fn filesystem_resolver_resolves_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.md"), "dotdot content").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let resolved = FilesystemFileResolver::new(None)
            .resolve(dir.path(), "subdir/../file.md")
            .expect("dotdot path should resolve");

        assert_eq!(resolved.content, "dotdot content");
    }

    #[test]
    fn filesystem_resolver_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared content").unwrap();

        let resolved = FilesystemFileResolver::new(Some(fallback.path().to_path_buf()))
            .resolve(base.path(), "shared.md")
            .expect("file should resolve from the fallback dir");

        assert_eq!(resolved.content, "shared content");
    }

    #[test]
    fn filesystem_resolver_base_dir_takes_precedence_over_fallback() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("prompt.md"), "base content").unwrap();
        std::fs::write(fallback.path().join("prompt.md"), "fallback content").unwrap();

        let resolved = FilesystemFileResolver::new(Some(fallback.path().to_path_buf()))
            .resolve(base.path(), "prompt.md")
            .expect("file should resolve from the base dir");

        assert_eq!(resolved.content, "base content");
    }

    #[test]
    fn filesystem_resolver_no_fallback_for_tilde_path() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("file.md"), "fallback").unwrap();

        // A tilde path to a nonexistent file does not fall back to the fallback dir.
        assert!(
            FilesystemFileResolver::new(Some(fallback.path().to_path_buf()))
                .resolve(base.path(), "~/nonexistent_fabro_test.md")
                .is_none()
        );
    }

    #[test]
    fn filesystem_resolver_returns_none_without_fallback() {
        let base = tempfile::tempdir().unwrap();

        assert!(
            FilesystemFileResolver::new(None)
                .resolve(base.path(), "missing.md")
                .is_none()
        );
    }
}
