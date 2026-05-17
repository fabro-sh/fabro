use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use fabro_types::ManifestPath;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateSource {
    pub path:    ManifestPath,
    pub content: String,
}

pub trait TemplateStore: Send + Sync {
    fn load(
        &self,
        parent: &ManifestPath,
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
    cwd:  PathBuf,
    root: ManifestPath,
}

impl FilesystemTemplateStore {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, root: ManifestPath) -> Self {
        Self {
            cwd: cwd.into(),
            root,
        }
    }
}

impl TemplateStore for FilesystemTemplateStore {
    #[expect(
        clippy::disallowed_methods,
        reason = "MiniJinja loaders are synchronous, so rooted template stores use sync file I/O"
    )]
    fn load(
        &self,
        parent: &ManifestPath,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let logical = resolve_logical_reference(parent, reference, &self.root)?;
        let absolute = self.cwd.join(logical.as_path());
        let root = self.cwd.join(self.root.as_path());

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
                parent:    parent.clone(),
                reference: reference.to_owned(),
                root:      self.root.clone(),
            });
        }

        let content =
            std::fs::read_to_string(&canonical).map_err(|source| TemplateLoadError::Io {
                path: canonical.clone(),
                source,
            })?;
        Ok(Some(TemplateSource {
            path: logical,
            content,
        }))
    }
}

#[derive(Clone, Debug, Default)]
pub struct BundleTemplateStore {
    root:  ManifestPath,
    files: HashMap<ManifestPath, String>,
}

impl BundleTemplateStore {
    #[must_use]
    pub fn new(root: ManifestPath, files: HashMap<ManifestPath, String>) -> Self {
        Self { root, files }
    }
}

impl TemplateStore for BundleTemplateStore {
    fn load(
        &self,
        parent: &ManifestPath,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let path = resolve_logical_reference(parent, reference, &self.root)?;
        Ok(self.files.get(&path).map(|content| TemplateSource {
            path,
            content: content.clone(),
        }))
    }
}

#[derive(Debug)]
pub struct CachedTemplateStore<T> {
    inner: T,
    cache: Mutex<HashMap<(ManifestPath, String), Option<TemplateSource>>>,
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
        parent: &ManifestPath,
        reference: &str,
    ) -> Result<Option<TemplateSource>, TemplateLoadError> {
        let key = (parent.clone(), reference.to_owned());
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
        parent: &ManifestPath,
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

pub(crate) fn resolve_logical_reference(
    parent: &ManifestPath,
    reference: &str,
    root: &ManifestPath,
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
    if !is_within_root(&path, root) {
        return Err(TemplateLoadError::EscapesRoot {
            parent:    parent.clone(),
            reference: reference.to_owned(),
            root:      root.clone(),
        });
    }
    Ok(path)
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
