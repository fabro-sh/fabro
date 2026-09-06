use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::marker::PhantomData;

use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{BlobHash, WorkflowPath, WorkflowVersionId};

pub const MAX_WORKFLOW_VERSION_FILES: usize = 512;
pub const MAX_WORKFLOW_VERSION_DEPENDENCIES: usize = 512;
pub const MAX_WORKFLOW_VERSION_FILE_BYTES: usize = 512 * 1024;
pub const MAX_WORKFLOW_VERSION_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum WorkflowVersionShapeError {
    #[error("workflow version has {actual} files; maximum is {maximum}")]
    TooManyFiles { actual: usize, maximum: usize },
    #[error("workflow version has {actual} workflow dependencies; maximum is {maximum}")]
    TooManyWorkflowDependencies { actual: usize, maximum: usize },
    #[error("workflow file `{path}` is {actual} bytes; maximum is {maximum}")]
    FileTooLarge {
        path:    WorkflowPath,
        actual:  usize,
        maximum: usize,
    },
    #[error("workflow version is {actual} canonical bytes; maximum is {maximum}")]
    VersionTooLarge { actual: usize, maximum: usize },
    #[error("entrypoint `{path}` is not present in workflow files")]
    MissingEntrypoint { path: WorkflowPath },
    #[error("workflow paths collide: `{first}` and `{second}`")]
    PathCollision {
        first:  WorkflowPath,
        second: WorkflowPath,
    },
    #[error("failed to serialize canonical workflow version")]
    Serialization {
        #[source]
        source: serde_json::Error,
    },
}

/// Canonical wire form of an immutable workflow version.
///
/// Construction (and therefore deserialization) enforces the structural
/// invariants: file-count and byte-size limits, entrypoint presence, unique
/// map keys, and collision-free paths. Semantic validation of graph, config,
/// and template content is a separate concern owned by
/// `fabro-workflow-version`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkflowVersion {
    entrypoint:            WorkflowPath,
    files:                 BTreeMap<WorkflowPath, String>,
    workflow_dependencies: BTreeMap<WorkflowPath, WorkflowVersionId>,
}

impl WorkflowVersion {
    pub fn new(
        entrypoint: WorkflowPath,
        files: BTreeMap<WorkflowPath, String>,
        workflow_dependencies: BTreeMap<WorkflowPath, WorkflowVersionId>,
    ) -> Result<Self, WorkflowVersionShapeError> {
        let version = Self {
            entrypoint,
            files,
            workflow_dependencies,
        };
        version.validate_shape()?;
        version.canonical_bytes()?;
        Ok(version)
    }

    #[must_use]
    pub fn entrypoint(&self) -> &WorkflowPath {
        &self.entrypoint
    }

    #[must_use]
    pub fn files(&self) -> &BTreeMap<WorkflowPath, String> {
        &self.files
    }

    #[must_use]
    pub fn workflow_dependencies(&self) -> &BTreeMap<WorkflowPath, WorkflowVersionId> {
        &self.workflow_dependencies
    }

    /// Path of the optional `workflow.toml` that configures this version. It
    /// always sits beside the entrypoint graph.
    #[must_use]
    pub fn config_path(&self) -> WorkflowPath {
        self.entrypoint
            .resolve_reference("workflow.toml")
            .expect("the static workflow config path must resolve beside a valid entrypoint")
    }

    /// Content-addressed identity: the hash of the canonical wire form.
    pub fn id(&self) -> Result<WorkflowVersionId, WorkflowVersionShapeError> {
        Ok(WorkflowVersionId::from(BlobHash::new(
            &self.canonical_bytes()?,
        )))
    }

    /// Serialize to the canonical wire form.
    ///
    /// Structural validity is guaranteed by construction, so this only
    /// serializes and enforces the canonical size limit.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, WorkflowVersionShapeError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|source| WorkflowVersionShapeError::Serialization { source })?;
        if bytes.len() > MAX_WORKFLOW_VERSION_BYTES {
            return Err(WorkflowVersionShapeError::VersionTooLarge {
                actual:  bytes.len(),
                maximum: MAX_WORKFLOW_VERSION_BYTES,
            });
        }
        Ok(bytes)
    }

    fn validate_shape(&self) -> Result<(), WorkflowVersionShapeError> {
        if self.files.len() > MAX_WORKFLOW_VERSION_FILES {
            return Err(WorkflowVersionShapeError::TooManyFiles {
                actual:  self.files.len(),
                maximum: MAX_WORKFLOW_VERSION_FILES,
            });
        }
        if self.workflow_dependencies.len() > MAX_WORKFLOW_VERSION_DEPENDENCIES {
            return Err(WorkflowVersionShapeError::TooManyWorkflowDependencies {
                actual:  self.workflow_dependencies.len(),
                maximum: MAX_WORKFLOW_VERSION_DEPENDENCIES,
            });
        }
        for (path, content) in &self.files {
            if content.len() > MAX_WORKFLOW_VERSION_FILE_BYTES {
                return Err(WorkflowVersionShapeError::FileTooLarge {
                    path:    path.clone(),
                    actual:  content.len(),
                    maximum: MAX_WORKFLOW_VERSION_FILE_BYTES,
                });
            }
        }
        if !self.files.contains_key(&self.entrypoint) {
            return Err(WorkflowVersionShapeError::MissingEntrypoint {
                path: self.entrypoint.clone(),
            });
        }
        self.validate_path_collisions()
    }

    fn validate_path_collisions(&self) -> Result<(), WorkflowVersionShapeError> {
        // Keys are unique within each map, so equality can only collide
        // across files and workflow dependencies.
        validate_workflow_path_collisions(
            self.files.keys().chain(self.workflow_dependencies.keys()),
        )
    }
}

/// Reject a set of workflow paths that cannot coexist on one filesystem: two
/// identical paths, or a path that is also an ancestor directory of another.
///
/// # Errors
///
/// Returns [`WorkflowVersionShapeError::PathCollision`] naming the two
/// conflicting paths.
pub fn validate_workflow_path_collisions<'a>(
    paths: impl IntoIterator<Item = &'a WorkflowPath> + Clone,
) -> Result<(), WorkflowVersionShapeError> {
    let mut by_text = HashMap::new();
    for path in paths.clone() {
        if let Some(existing) = by_text.insert(path.as_str(), path) {
            return Err(WorkflowVersionShapeError::PathCollision {
                first:  existing.clone(),
                second: path.clone(),
            });
        }
    }
    for path in paths {
        let text = path.as_str();
        for (index, _) in text.match_indices('/') {
            if let Some(ancestor) = by_text.get(&text[..index]) {
                return Err(WorkflowVersionShapeError::PathCollision {
                    first:  (*ancestor).clone(),
                    second: path.clone(),
                });
            }
        }
    }
    Ok(())
}

impl<'de> Deserialize<'de> for WorkflowVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            entrypoint:            WorkflowPath,
            files:                 UniqueBTreeMap<WorkflowPath, String>,
            workflow_dependencies: UniqueBTreeMap<WorkflowPath, WorkflowVersionId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.entrypoint, wire.files.0, wire.workflow_dependencies.0)
            .map_err(D::Error::custom)
    }
}

struct UniqueBTreeMap<K, V>(BTreeMap<K, V>);

impl<'de, K, V> Deserialize<'de> for UniqueBTreeMap<K, V>
where
    K: Deserialize<'de> + Ord + fmt::Display,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for MapVisitor<K, V>
        where
            K: Deserialize<'de> + Ord + fmt::Display,
            V: Deserialize<'de>,
        {
            type Value = UniqueBTreeMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map with unique keys")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<K, V>()? {
                    if values.insert(key, value).is_some() {
                        return Err(A::Error::custom("duplicate workflow map key"));
                    }
                }
                Ok(UniqueBTreeMap(values))
            }
        }

        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        MAX_WORKFLOW_VERSION_BYTES, MAX_WORKFLOW_VERSION_DEPENDENCIES,
        MAX_WORKFLOW_VERSION_FILE_BYTES, MAX_WORKFLOW_VERSION_FILES, WorkflowVersion,
        WorkflowVersionShapeError,
    };
    use crate::{BlobHash, WorkflowPath, WorkflowVersionId};

    fn path(value: &str) -> WorkflowPath {
        value.parse().unwrap()
    }

    #[test]
    fn canonical_bytes_have_fixed_field_and_map_order() {
        let version = WorkflowVersion::new(
            path("workflow.fabro"),
            BTreeMap::from([
                (path("z.txt"), "Z".to_string()),
                (path("workflow.fabro"), "digraph W {}".to_string()),
                (path("a.txt"), "A".to_string()),
            ]),
            BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(version.canonical_bytes().unwrap()).unwrap(),
            r#"{"entrypoint":"workflow.fabro","files":{"a.txt":"A","workflow.fabro":"digraph W {}","z.txt":"Z"},"workflow_dependencies":{}}"#
        );
    }

    #[test]
    fn rejects_missing_entrypoint() {
        let error = WorkflowVersion::new(
            path("missing.fabro"),
            BTreeMap::from([(path("workflow.fabro"), "digraph W {}".to_string())]),
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            WorkflowVersionShapeError::MissingEntrypoint { .. }
        ));
    }

    #[test]
    fn rejects_path_collisions_and_large_files() {
        let collision = WorkflowVersion::new(
            path("workflow.fabro"),
            BTreeMap::from([
                (path("workflow.fabro"), "digraph W {}".to_string()),
                (path("assets"), "file".to_string()),
                (path("assets/item.txt"), "nested".to_string()),
            ]),
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(
            collision,
            WorkflowVersionShapeError::PathCollision { .. }
        ));

        let mut files = BTreeMap::from([(path("workflow.fabro"), "digraph W {}".to_string())]);
        files.insert(
            path("large.txt"),
            "x".repeat(MAX_WORKFLOW_VERSION_FILE_BYTES + 1),
        );
        let large =
            WorkflowVersion::new(path("workflow.fabro"), files, BTreeMap::new()).unwrap_err();
        assert!(matches!(
            large,
            WorkflowVersionShapeError::FileTooLarge { .. }
        ));
    }

    #[test]
    fn rejects_ancestor_collisions_hidden_by_sort_order() {
        // `assets.txt` sorts between `assets` and `assets/item.txt` because
        // '.' precedes '/', so an adjacent-pair scan over the sorted list
        // would miss this collision.
        let error = WorkflowVersion::new(
            path("workflow.fabro"),
            BTreeMap::from([
                (path("workflow.fabro"), "digraph W {}".to_string()),
                (path("assets"), "file".to_string()),
                (path("assets.txt"), "sibling".to_string()),
                (path("assets/item.txt"), "nested".to_string()),
            ]),
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            WorkflowVersionShapeError::PathCollision { first, second }
                if first.as_str() == "assets" && second.as_str() == "assets/item.txt"
        ));

        assert!(
            WorkflowVersion::new(
                path("workflow.fabro"),
                BTreeMap::from([
                    (path("workflow.fabro"), "digraph W {}".to_string()),
                    (path("assets.txt"), "sibling".to_string()),
                    (path("assets/item.txt"), "nested".to_string()),
                ]),
                BTreeMap::new(),
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_collisions_across_files_and_workflow_dependencies() {
        let dependency_id = WorkflowVersionId::from(BlobHash::new(b"child"));

        let equal = WorkflowVersion::new(
            path("workflow.fabro"),
            BTreeMap::from([
                (path("workflow.fabro"), "digraph W {}".to_string()),
                (path("child.fabro"), "digraph C {}".to_string()),
            ]),
            BTreeMap::from([(path("child.fabro"), dependency_id)]),
        )
        .unwrap_err();
        assert!(matches!(
            equal,
            WorkflowVersionShapeError::PathCollision { first, second }
                if first == second && first.as_str() == "child.fabro"
        ));

        let ancestor = WorkflowVersion::new(
            path("workflow.fabro"),
            BTreeMap::from([
                (path("workflow.fabro"), "digraph W {}".to_string()),
                (path("libs"), "file".to_string()),
                (path("libs.md"), "sibling".to_string()),
            ]),
            BTreeMap::from([(path("libs/child.fabro"), dependency_id)]),
        )
        .unwrap_err();
        assert!(matches!(
            ancestor,
            WorkflowVersionShapeError::PathCollision { first, second }
                if first.as_str() == "libs" && second.as_str() == "libs/child.fabro"
        ));
    }

    #[test]
    fn enforces_file_count_file_size_and_canonical_size_boundaries() {
        let mut files = BTreeMap::from([(path("workflow.fabro"), "digraph W {}".to_string())]);
        for index in 0..MAX_WORKFLOW_VERSION_FILES - 1 {
            files.insert(path(&format!("file-{index:03}.txt")), String::new());
        }
        assert!(
            WorkflowVersion::new(path("workflow.fabro"), files.clone(), BTreeMap::new()).is_ok()
        );
        files.insert(path("too-many.txt"), String::new());
        assert!(matches!(
            WorkflowVersion::new(path("workflow.fabro"), files, BTreeMap::new()).unwrap_err(),
            WorkflowVersionShapeError::TooManyFiles { .. }
        ));

        let exact_file = BTreeMap::from([
            (path("workflow.fabro"), "digraph W {}".to_string()),
            (
                path("payload.txt"),
                "x".repeat(MAX_WORKFLOW_VERSION_FILE_BYTES),
            ),
        ]);
        assert!(
            WorkflowVersion::new(path("workflow.fabro"), exact_file.clone(), BTreeMap::new())
                .is_ok()
        );
        let mut oversized_file = exact_file;
        oversized_file
            .get_mut(&path("payload.txt"))
            .unwrap()
            .push('x');
        assert!(matches!(
            WorkflowVersion::new(path("workflow.fabro"), oversized_file, BTreeMap::new())
                .unwrap_err(),
            WorkflowVersionShapeError::FileTooLarge { .. }
        ));

        let mut exact_version_files =
            BTreeMap::from([(path("workflow.fabro"), "digraph W {}".to_string())]);
        for index in 0..4 {
            exact_version_files.insert(path(&format!("payload-{index}.txt")), String::new());
        }
        let empty = WorkflowVersion::new(
            path("workflow.fabro"),
            exact_version_files.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        let remaining = MAX_WORKFLOW_VERSION_BYTES - empty.canonical_bytes().unwrap().len();
        let per_file = remaining / 4;
        let remainder = remaining % 4;
        for index in 0..4 {
            let length = per_file + usize::from(index < remainder);
            assert!(length <= MAX_WORKFLOW_VERSION_FILE_BYTES);
            exact_version_files.insert(path(&format!("payload-{index}.txt")), "x".repeat(length));
        }
        let exact_version = WorkflowVersion::new(
            path("workflow.fabro"),
            exact_version_files.clone(),
            BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            exact_version.canonical_bytes().unwrap().len(),
            MAX_WORKFLOW_VERSION_BYTES
        );
        exact_version_files
            .get_mut(&path("payload-0.txt"))
            .unwrap()
            .push('x');
        assert!(matches!(
            WorkflowVersion::new(path("workflow.fabro"), exact_version_files, BTreeMap::new())
                .unwrap_err(),
            WorkflowVersionShapeError::VersionTooLarge { .. }
        ));
    }

    #[test]
    fn enforces_workflow_dependency_count_boundary() {
        let dependencies = (0..MAX_WORKFLOW_VERSION_DEPENDENCIES)
            .map(|index| {
                (
                    path(&format!("dependency-{index:03}.fabro")),
                    WorkflowVersionId::from(BlobHash::new(index.to_string().as_bytes())),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let files = BTreeMap::from([(path("workflow.fabro"), "digraph W {}".to_owned())]);
        assert!(
            WorkflowVersion::new(path("workflow.fabro"), files.clone(), dependencies.clone())
                .is_ok()
        );

        let mut oversized = dependencies;
        oversized.insert(
            path("too-many.fabro"),
            WorkflowVersionId::from(BlobHash::new(b"too many")),
        );
        assert!(matches!(
            WorkflowVersion::new(path("workflow.fabro"), files, oversized).unwrap_err(),
            WorkflowVersionShapeError::TooManyWorkflowDependencies { .. }
        ));
    }

    #[test]
    fn deserialize_rejects_unknown_fields_and_duplicate_keys() {
        let unknown = r#"{
            "entrypoint":"workflow.fabro",
            "files":{"workflow.fabro":"digraph W {}"},
            "workflow_dependencies":{},
            "metadata":{}
        }"#;
        assert!(serde_json::from_str::<WorkflowVersion>(unknown).is_err());

        let duplicate = r#"{
            "entrypoint":"workflow.fabro",
            "files":{"workflow.fabro":"digraph W {}","workflow.fabro":"digraph X {}"},
            "workflow_dependencies":{}
        }"#;
        assert!(serde_json::from_str::<WorkflowVersion>(duplicate).is_err());
    }
}
