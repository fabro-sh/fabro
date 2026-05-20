use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use fabro_types::ManifestPath;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateSourceOrigin {
    source_text:    Arc<str>,
    fragment_start: usize,
}

impl TemplateSourceOrigin {
    #[must_use]
    fn new(source_text: impl Into<Arc<str>>, fragment_start: usize) -> Self {
        Self {
            source_text: source_text.into(),
            fragment_start,
        }
    }

    #[must_use]
    pub fn from_first_fragment_match(source_text: &str, fragment: &str) -> Option<Self> {
        source_text
            .find(fragment)
            .map(|fragment_start| Self::new(source_text, fragment_start))
    }

    #[must_use]
    pub(crate) fn source_text(&self) -> &str {
        &self.source_text
    }

    #[must_use]
    pub(crate) fn clone_source_text(&self) -> Arc<str> {
        Arc::clone(&self.source_text)
    }

    #[must_use]
    pub(crate) fn fragment_start(&self) -> usize {
        self.fragment_start
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateSource {
    pub path:    ManifestPath,
    pub root:    ManifestPath,
    pub content: String,
    pub origin:  Option<TemplateSourceOrigin>,
}

impl TemplateSource {
    #[must_use]
    pub fn new(path: ManifestPath, root: ManifestPath, content: impl Into<String>) -> Self {
        Self {
            path,
            root,
            content: content.into(),
            origin: None,
        }
    }

    #[must_use]
    pub fn with_origin(mut self, origin: TemplateSourceOrigin) -> Self {
        self.origin = Some(origin);
        self
    }
}

pub trait TemplateStore: Send + Sync {
    fn load(
        &self,
        parent: &TemplateSource,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError>;
}

#[derive(Debug, Error)]
pub enum TemplateLoadError {
    #[error("unsafe template reference `{reference}` from `{parent}`")]
    UnsafeReference {
        parent:    ManifestPath,
        reference: String,
    },
    #[error("template reference `{reference}` from `{parent}` escapes template root `{root}`")]
    EscapesRoot {
        parent:    ManifestPath,
        reference: String,
        root:      ManifestPath,
    },
    #[error("failed to read template `{path}`")]
    Io {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("dynamic template dependency `{path}` is not declared as an asset")]
    DynamicDependency { path: ManifestPath },
}

#[derive(Clone, Debug)]
pub struct FilesystemTemplateStore {
    cwd: PathBuf,
}

impl FilesystemTemplateStore {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }
}

impl TemplateStore for FilesystemTemplateStore {
    #[expect(
        clippy::disallowed_methods,
        reason = "MiniJinja loaders are synchronous, so rooted template stores use sync file I/O"
    )]
    fn load(
        &self,
        parent: &TemplateSource,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let logical =
            TemplateIncludeResolver::new(parent.root.clone()).resolve(&parent.path, reference)?;
        let absolute = self.cwd.join(logical.as_path());
        let root = self.cwd.join(parent.root.as_path());

        let canonical = match absolute.canonicalize() {
            Ok(path) if path.is_file() => path,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(TemplateLoadError::Io {
                    path:   absolute,
                    source: error,
                });
            }
        };
        let canonical_root = root
            .canonicalize()
            .map_err(|source| TemplateLoadError::Io {
                path: root.clone(),
                source,
            })?;
        if !canonical.starts_with(&canonical_root) {
            return Err(TemplateLoadError::EscapesRoot {
                parent:    parent.path.clone(),
                reference: reference.to_owned(),
                root:      parent.root.clone(),
            });
        }

        let content =
            std::fs::read_to_string(&canonical).map_err(|source| TemplateLoadError::Io {
                path: canonical.clone(),
                source,
            })?;
        Ok(Some(TemplateSource::new(
            logical,
            parent.root.clone(),
            content,
        )))
    }
}

#[derive(Clone, Debug, Default)]
pub struct BundleTemplateStore {
    files: HashMap<ManifestPath, String>,
}

impl BundleTemplateStore {
    #[must_use]
    pub fn new(files: HashMap<ManifestPath, String>) -> Self {
        Self { files }
    }
}

impl TemplateStore for BundleTemplateStore {
    fn load(
        &self,
        parent: &TemplateSource,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let path =
            TemplateIncludeResolver::new(parent.root.clone()).resolve(&parent.path, reference)?;
        Ok(self
            .files
            .get(&path)
            .map(|content| TemplateSource::new(path, parent.root.clone(), content.clone())))
    }
}

#[derive(Debug)]
pub struct CachedTemplateStore<T> {
    inner: T,
    cache: Mutex<HashMap<(ManifestPath, ManifestPath, String), Option<TemplateSource>>>,
}

impl<T> CachedTemplateStore<T> {
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> TemplateStore for CachedTemplateStore<T>
where
    T: TemplateStore,
{
    fn load(
        &self,
        parent: &TemplateSource,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let key = (
            parent.path.clone(),
            parent.root.clone(),
            reference.to_owned(),
        );
        if let Some(source) = lock(&self.cache).get(&key).cloned() {
            return Ok(source);
        }
        let source = self.inner.load(parent, reference)?;
        lock(&self.cache).insert(key, source.clone());
        Ok(source)
    }
}

#[derive(Debug)]
pub struct RecordingTemplateStore<T> {
    inner:   T,
    loaded:  Mutex<HashSet<ManifestPath>>,
    allowed: Option<HashSet<ManifestPath>>,
}

impl<T> RecordingTemplateStore<T> {
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            loaded: Mutex::new(HashSet::new()),
            allowed: None,
        }
    }

    #[must_use]
    pub fn with_allowed(inner: T, allowed: HashSet<ManifestPath>) -> Self {
        Self {
            inner,
            loaded: Mutex::new(HashSet::new()),
            allowed: Some(allowed),
        }
    }

    #[must_use]
    pub fn loaded_paths(&self) -> HashSet<ManifestPath> {
        lock(&self.loaded).clone()
    }
}

impl<T> TemplateStore for RecordingTemplateStore<T>
where
    T: TemplateStore,
{
    fn load(
        &self,
        parent: &TemplateSource,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let source = self.inner.load(parent, reference)?;
        if let Some(source) = source.as_ref() {
            if let Some(allowed) = &self.allowed {
                if !allowed.contains(&source.path) {
                    return Err(TemplateLoadError::DynamicDependency {
                        path: source.path.clone(),
                    });
                }
            }
            lock(&self.loaded).insert(source.path.clone());
        }
        Ok(source)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateIncludeResolver {
    root: ManifestPath,
}

impl TemplateIncludeResolver {
    #[must_use]
    pub fn new(root: ManifestPath) -> Self {
        Self { root }
    }

    pub fn resolve(
        &self,
        parent: &ManifestPath,
        reference: &str,
    ) -> Result<ManifestPath, TemplateLoadError> {
        if !is_safe_template_reference(reference) {
            return Err(TemplateLoadError::UnsafeReference {
                parent:    parent.clone(),
                reference: reference.to_owned(),
            });
        }
        let path =
            ManifestPath::from_reference(parent.parent_or_dot(), reference).ok_or_else(|| {
                TemplateLoadError::UnsafeReference {
                    parent:    parent.clone(),
                    reference: reference.to_owned(),
                }
            })?;
        if !is_within_root(&path, &self.root) {
            return Err(TemplateLoadError::EscapesRoot {
                parent:    parent.clone(),
                reference: reference.to_owned(),
                root:      self.root.clone(),
            });
        }
        Ok(path)
    }
}

pub(crate) fn is_safe_template_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('~')
        && !reference.contains('\\')
        && !has_windows_drive_prefix(reference)
        && !Path::new(reference).is_absolute()
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let mut chars = path.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(first), Some(':')) if first.is_ascii_alphabetic()
    )
}

fn is_within_root(path: &ManifestPath, root: &ManifestPath) -> bool {
    if root.as_path().as_os_str().is_empty() {
        return !matches!(
            path.as_path().components().next(),
            Some(Component::ParentDir)
        );
    }
    path.starts_with(root)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("template store mutex should not be poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("path should parse")
    }

    #[test]
    fn include_resolver_allows_sibling_reference_within_root() {
        let resolver = TemplateIncludeResolver::new(manifest_path("prompts"));

        let resolved = resolver
            .resolve(
                &manifest_path("prompts/audits/audit.prompt.md"),
                "../partials/audit.partial.tpl",
            )
            .unwrap();

        assert_eq!(
            resolved,
            manifest_path("prompts/partials/audit.partial.tpl")
        );
    }

    #[test]
    fn include_resolver_rejects_reference_that_escapes_root() {
        let resolver = TemplateIncludeResolver::new(manifest_path("prompts"));

        let err = resolver
            .resolve(
                &manifest_path("prompts/audits/audit.prompt.md"),
                "../../secret.md",
            )
            .unwrap_err();

        assert!(matches!(
            err,
            TemplateLoadError::EscapesRoot {
                parent,
                reference,
                root,
            } if parent == manifest_path("prompts/audits/audit.prompt.md")
                && reference == "../../secret.md"
                && root == manifest_path("prompts")
        ));
    }

    #[test]
    fn include_resolver_rejects_unsafe_references() {
        let resolver = TemplateIncludeResolver::new(manifest_path("prompts"));
        let parent = manifest_path("prompts/main.md");

        for reference in [
            "",
            "~/secret.md",
            "/tmp/secret.md",
            r"partials\secret.md",
            "C:/secret.md",
        ] {
            let err = resolver.resolve(&parent, reference).unwrap_err();
            assert!(
                matches!(err, TemplateLoadError::UnsafeReference { .. }),
                "{reference} should be unsafe, got {err:?}"
            );
        }
    }
}
