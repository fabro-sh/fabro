use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A path used as a key inside a run manifest. It is anchored at the run's
/// cwd, and may contain leading `..` segments for files outside that cwd.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct ManifestPath(PathBuf);

impl ManifestPath {
    #[must_use]
    pub fn from_reference(current_dir: &Path, reference: &str) -> Option<Self> {
        let path = Path::new(reference);
        if path.is_absolute() || reference.starts_with('~') {
            return None;
        }

        normalize_components(current_dir.join(path)).map(Self)
    }

    #[must_use]
    pub fn from_absolute(absolute: &Path, cwd: &Path) -> Option<Self> {
        if let Ok(stripped) = absolute.strip_prefix(cwd) {
            return normalize_components(stripped).map(Self);
        }

        relative_path_from(absolute, cwd).and_then(|path| normalize_components(path).map(Self))
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::from_reference(Path::new("."), value)
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn parent(&self) -> Option<&Path> {
        self.0.parent()
    }

    /// Directory that contains this path, falling back to `.` when the path
    /// has no parent component (e.g. a bare file name).
    #[must_use]
    pub fn parent_or_dot(&self) -> &Path {
        self.0.parent().unwrap_or_else(|| Path::new("."))
    }
}

impl From<ManifestPath> for PathBuf {
    fn from(value: ManifestPath) -> Self {
        value.0
    }
}

impl TryFrom<String> for ManifestPath {
    type Error = ManifestPathParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_wire(&value).ok_or(ManifestPathParseError(value))
    }
}

impl From<ManifestPath> for String {
    fn from(value: ManifestPath) -> Self {
        value.to_string()
    }
}

impl fmt::Display for ManifestPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

#[derive(Debug)]
pub struct ManifestPathParseError(String);

impl fmt::Display for ManifestPathParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid ManifestPath: {}", self.0)
    }
}

impl std::error::Error for ManifestPathParseError {}

fn normalize_components(path: impl AsRef<Path>) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.as_ref().components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                } else {
                    normalized.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn relative_path_from(path: &Path, base: &Path) -> Option<PathBuf> {
    let path_components = path.components().collect::<Vec<_>>();
    let base_components = base.components().collect::<Vec<_>>();
    if path_components.is_empty() || base_components.is_empty() {
        return None;
    }

    let mut common = 0;
    while common < path_components.len()
        && common < base_components.len()
        && path_components[common] == base_components[common]
    {
        common += 1;
    }

    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &path_components[common..] {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => relative.push(".."),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(relative)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("path should parse")
    }

    #[test]
    fn from_reference_rejects_absolute_path() {
        assert!(ManifestPath::from_reference(Path::new("."), "/tmp/workflow.fabro").is_none());
    }

    #[test]
    fn from_reference_rejects_tilde_reference() {
        assert!(ManifestPath::from_reference(Path::new("."), "~/.fabro/workflow.fabro").is_none());
    }

    #[test]
    fn from_reference_simple_relative() {
        let path = ManifestPath::from_reference(Path::new("flows"), "workflow.fabro").unwrap();

        assert_eq!(path.as_path(), Path::new("flows/workflow.fabro"));
    }

    #[test]
    fn from_reference_collapses_curdir_segments() {
        let path = ManifestPath::from_reference(Path::new("./flows"), "./workflow.fabro").unwrap();

        assert_eq!(path.as_path(), Path::new("flows/workflow.fabro"));
    }

    #[test]
    fn from_reference_collapses_mid_path_parent_dir() {
        let path = ManifestPath::from_reference(Path::new("../foo"), "../bar/file.md").unwrap();

        assert_eq!(path.as_path(), Path::new("../bar/file.md"));
    }

    #[test]
    fn from_reference_preserves_single_leading_parent_dir() {
        let path =
            ManifestPath::from_reference(Path::new("../.fabro/workflows/demo"), "prompts/hello.md")
                .unwrap();

        assert_eq!(
            path.as_path(),
            Path::new("../.fabro/workflows/demo/prompts/hello.md")
        );
    }

    #[test]
    fn from_reference_preserves_multiple_leading_parent_dirs() {
        let path =
            ManifestPath::from_reference(Path::new("../../shared/workflows"), "file.md").unwrap();

        assert_eq!(path.as_path(), Path::new("../../shared/workflows/file.md"));
    }

    #[test]
    fn from_reference_collapses_then_escapes() {
        let path = ManifestPath::from_reference(Path::new("."), "foo/../..").unwrap();

        assert_eq!(path.as_path(), Path::new(".."));
    }

    #[test]
    fn from_absolute_file_inside_cwd_strips_prefix() {
        let path = ManifestPath::from_absolute(
            Path::new("/repo/project/workflow.fabro"),
            Path::new("/repo/project"),
        )
        .unwrap();

        assert_eq!(path.as_path(), Path::new("workflow.fabro"));
    }

    #[test]
    fn from_absolute_sibling_directory_uses_single_parent_dir() {
        let path = ManifestPath::from_absolute(
            Path::new("/repo/shared/workflow.fabro"),
            Path::new("/repo/project"),
        )
        .unwrap();

        assert_eq!(path.as_path(), Path::new("../shared/workflow.fabro"));
    }

    #[test]
    fn from_absolute_grandparent_uses_two_parent_dirs() {
        let path = ManifestPath::from_absolute(
            Path::new("/repo/shared/workflow.fabro"),
            Path::new("/repo/project/nested"),
        )
        .unwrap();

        assert_eq!(path.as_path(), Path::new("../../shared/workflow.fabro"));
    }

    #[test]
    fn from_absolute_user_global_workflow_from_unrelated_cwd() {
        let path = ManifestPath::from_absolute(
            Path::new("/tmp/.fabro/workflows/demo/workflow.fabro"),
            Path::new("/tmp/project"),
        )
        .unwrap();

        assert_eq!(
            path.as_path(),
            Path::new("../.fabro/workflows/demo/workflow.fabro")
        );
    }

    #[test]
    fn from_wire_accepts_canonical_relative() {
        let path = ManifestPath::from_wire("workflow.fabro").unwrap();

        assert_eq!(path.as_path(), Path::new("workflow.fabro"));
    }

    #[test]
    fn from_wire_rejects_absolute_path() {
        assert!(ManifestPath::from_wire("/tmp/workflow.fabro").is_none());
    }

    #[test]
    fn from_wire_rejects_tilde_path() {
        assert!(ManifestPath::from_wire("~/.fabro/workflow.fabro").is_none());
    }

    #[test]
    fn from_wire_renormalizes_uncollapsed_curdir() {
        let path = ManifestPath::from_wire("./foo/./bar").unwrap();

        assert_eq!(path.as_path(), Path::new("foo/bar"));
    }

    #[test]
    fn from_wire_renormalizes_mid_path_parent_dir() {
        let path = ManifestPath::from_wire("foo/../bar").unwrap();

        assert_eq!(path.as_path(), Path::new("bar"));
    }

    #[test]
    fn from_wire_preserves_leading_parent_dir() {
        let path = ManifestPath::from_wire("../.fabro/workflows/demo/workflow.fabro").unwrap();

        assert_eq!(
            path.as_path(),
            Path::new("../.fabro/workflows/demo/workflow.fabro")
        );
    }

    #[test]
    fn deserialize_rejects_absolute_path() {
        assert!(serde_json::from_str::<ManifestPath>("\"/abs\"").is_err());
    }

    #[test]
    fn deserialize_rejects_tilde_path() {
        assert!(serde_json::from_str::<ManifestPath>("\"~/.fabro/x\"").is_err());
    }

    #[test]
    fn deserialize_renormalizes_non_canonical() {
        let path: ManifestPath = serde_json::from_str("\"foo/../bar\"").unwrap();

        assert_eq!(path, manifest_path("bar"));
    }

    #[test]
    fn round_trip_file_inside_cwd() {
        let produced = ManifestPath::from_absolute(
            Path::new("/repo/project/prompts/hello.md"),
            Path::new("/repo/project"),
        )
        .unwrap();
        let consumed = ManifestPath::from_reference(Path::new("prompts"), "hello.md").unwrap();

        assert_eq!(produced, consumed);
    }

    #[test]
    fn round_trip_user_global_workflow() {
        let produced = ManifestPath::from_absolute(
            Path::new("/tmp/.fabro/workflows/demo/prompts/hello.md"),
            Path::new("/tmp/project"),
        )
        .unwrap();
        let workflow = ManifestPath::from_absolute(
            Path::new("/tmp/.fabro/workflows/demo/workflow.fabro"),
            Path::new("/tmp/project"),
        )
        .unwrap();
        let consumed =
            ManifestPath::from_reference(workflow.parent().unwrap(), "prompts/hello.md").unwrap();

        assert_eq!(produced, consumed);
    }

    #[test]
    fn round_trip_subworkflow_relative_reference() {
        let produced = ManifestPath::from_absolute(
            Path::new("/repo/.fabro/workflows/child/workflow.fabro"),
            Path::new("/repo"),
        )
        .unwrap();
        let root = ManifestPath::from_absolute(
            Path::new("/repo/.fabro/workflows/demo/workflow.fabro"),
            Path::new("/repo"),
        )
        .unwrap();
        let consumed =
            ManifestPath::from_reference(root.parent().unwrap(), "../child/workflow.fabro")
                .unwrap();

        assert_eq!(produced, consumed);
    }

    #[test]
    fn serializes_as_plain_string() {
        let serialized = serde_json::to_string(&manifest_path("../workflow.fabro")).unwrap();

        assert_eq!(serialized, "\"../workflow.fabro\"");
    }

    #[test]
    fn deserializes_from_plain_string() {
        let path: ManifestPath = serde_json::from_str("\"workflow.fabro\"").unwrap();

        assert_eq!(path.as_path(), Path::new("workflow.fabro"));
    }
}
