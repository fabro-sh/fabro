use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use fabro_store::BlobStore;
use fabro_types::{WorkflowPath, WorkflowVersion, WorkflowVersionId, WorkflowVersionShapeError};
use thiserror::Error;

use crate::{ValidatedWorkflowVersion, WorkflowVersionError};

#[derive(Debug, Error)]
pub enum WorkflowVersionStoreError {
    #[error(transparent)]
    InvalidVersion(#[from] WorkflowVersionError),
    #[error(transparent)]
    InvalidShape(#[from] WorkflowVersionShapeError),
    #[error("workflow-version dependency `{id}` at `{path}` is not stored")]
    DependencyNotFound {
        path: WorkflowPath,
        id:   WorkflowVersionId,
    },
    #[error("workflow-version dependency `{id}` at `{path}` is invalid")]
    DependencyInvalid {
        path:   WorkflowPath,
        id:     WorkflowVersionId,
        #[source]
        source: Box<Self>,
    },
    #[error("workflow-version blob `{id}` cannot be decoded as a valid workflow version")]
    Decode {
        id:     WorkflowVersionId,
        #[source]
        source: serde_json::Error,
    },
    #[error("workflow-version blob `{id}` is not canonical")]
    NonCanonical { id: WorkflowVersionId },
    #[error("workflow-version storage operation failed")]
    Storage {
        #[source]
        source: fabro_store::Error,
    },
}

/// A fully loaded and validated workflow-version dependency graph: the
/// requested root alongside every unique transitive dependency, keyed by
/// canonical content ID.
///
/// Deliberately not `Clone`: a closure owns the full file contents of every
/// version in the graph, so copies should be explicit and deliberate.
#[derive(Debug)]
pub struct LoadedWorkflowVersionClosure {
    root_id:      WorkflowVersionId,
    root:         ValidatedWorkflowVersion,
    dependencies: BTreeMap<WorkflowVersionId, ValidatedWorkflowVersion>,
}

impl LoadedWorkflowVersionClosure {
    #[must_use]
    pub fn root_id(&self) -> WorkflowVersionId {
        self.root_id
    }

    #[must_use]
    pub fn root(&self) -> &WorkflowVersion {
        self.root.version()
    }

    /// The certified root version, for consumers that need guarantees only
    /// [`ValidatedWorkflowVersion`] provides (e.g. goal-file resolution).
    #[must_use]
    pub fn validated_root(&self) -> &ValidatedWorkflowVersion {
        &self.root
    }

    #[must_use]
    pub fn get(&self, id: &WorkflowVersionId) -> Option<&WorkflowVersion> {
        if *id == self.root_id {
            return Some(self.root.version());
        }
        self.dependencies
            .get(id)
            .map(ValidatedWorkflowVersion::version)
    }

    pub fn versions(&self) -> impl Iterator<Item = (WorkflowVersionId, &WorkflowVersion)> + '_ {
        std::iter::once((self.root_id, self.root.version())).chain(
            self.dependencies
                .iter()
                .map(|(id, version)| (*id, version.version())),
        )
    }
}

/// Content-addressed storage for validated workflow versions.
///
/// `put` only accepts semantically validated versions; `get` re-validates
/// blobs on read because the blob namespace is shared and storage is not
/// trusted to contain only canonical versions.
#[derive(Clone, Debug)]
pub struct WorkflowVersionStore {
    blobs: Arc<BlobStore>,
}

impl WorkflowVersionStore {
    #[must_use]
    pub fn new(blobs: Arc<BlobStore>) -> Self {
        Self { blobs }
    }

    pub async fn put(
        &self,
        version: &ValidatedWorkflowVersion,
    ) -> Result<WorkflowVersionId, WorkflowVersionStoreError> {
        let canonical = version.version().canonical_bytes()?;
        self.walk_dependency_closure(version.version().workflow_dependencies(), |_, _| ())
            .await?;
        self.blobs
            .write(&canonical)
            .await
            .map(WorkflowVersionId::from)
            .map_err(|source| WorkflowVersionStoreError::Storage { source })
    }

    pub async fn get(
        &self,
        id: &WorkflowVersionId,
    ) -> Result<Option<ValidatedWorkflowVersion>, WorkflowVersionStoreError> {
        let Some(version) = self.load_one(id).await? else {
            return Ok(None);
        };
        self.walk_dependency_closure(version.version().workflow_dependencies(), |_, _| ())
            .await?;
        Ok(Some(version))
    }

    pub async fn get_closure(
        &self,
        root_id: &WorkflowVersionId,
    ) -> Result<Option<LoadedWorkflowVersionClosure>, WorkflowVersionStoreError> {
        let Some(root) = self.load_one(root_id).await? else {
            return Ok(None);
        };
        let mut dependencies = BTreeMap::new();
        self.walk_dependency_closure(root.version().workflow_dependencies(), |id, version| {
            dependencies.insert(id, version);
        })
        .await?;
        Ok(Some(LoadedWorkflowVersionClosure {
            root_id: *root_id,
            root,
            dependencies,
        }))
    }

    async fn load_one(
        &self,
        id: &WorkflowVersionId,
    ) -> Result<Option<ValidatedWorkflowVersion>, WorkflowVersionStoreError> {
        let blob_hash = (*id).into();
        let Some(bytes) = self
            .blobs
            .read(&blob_hash)
            .await
            .map_err(|source| WorkflowVersionStoreError::Storage { source })?
        else {
            return Ok(None);
        };
        let version = serde_json::from_slice::<WorkflowVersion>(&bytes)
            .map_err(|source| WorkflowVersionStoreError::Decode { id: *id, source })?;
        let validated = ValidatedWorkflowVersion::new(version)?;
        let canonical = validated.version().canonical_bytes()?;
        if canonical.as_slice() != bytes.as_ref() {
            return Err(WorkflowVersionStoreError::NonCanonical { id: *id });
        }
        Ok(Some(validated))
    }

    /// Walk the transitive dependency closure, validating every dependency
    /// and handing each loaded version to `visit` exactly once.
    async fn walk_dependency_closure(
        &self,
        dependencies: &BTreeMap<WorkflowPath, WorkflowVersionId>,
        mut visit: impl FnMut(WorkflowVersionId, ValidatedWorkflowVersion),
    ) -> Result<(), WorkflowVersionStoreError> {
        let mut pending = dependencies
            .iter()
            .map(|(path, id)| (path.clone(), *id))
            .collect::<VecDeque<_>>();
        let mut visited = HashSet::new();

        while let Some((path, id)) = pending.pop_front() {
            if !visited.insert(id) {
                continue;
            }
            match self.load_one(&id).await {
                Ok(Some(dependency)) => {
                    pending.extend(
                        dependency
                            .version()
                            .workflow_dependencies()
                            .iter()
                            .map(|(path, id)| (path.clone(), *id)),
                    );
                    visit(id, dependency);
                }
                Ok(None) => {
                    return Err(WorkflowVersionStoreError::DependencyNotFound { path, id });
                }
                // Persistence failures are server faults, not evidence that
                // the caller supplied an invalid dependency.
                Err(source @ WorkflowVersionStoreError::Storage { .. }) => return Err(source),
                Err(source) => {
                    return Err(WorkflowVersionStoreError::DependencyInvalid {
                        path,
                        id,
                        source: Box::new(source),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use fabro_store::{BlobStore, test_support};
    use fabro_types::{WorkflowPath, WorkflowVersion, WorkflowVersionId};

    use super::{WorkflowVersionStore, WorkflowVersionStoreError};
    use crate::ValidatedWorkflowVersion;

    fn path(value: &str) -> WorkflowPath {
        value.parse().unwrap()
    }

    fn version(
        graph: &str,
        dependencies: BTreeMap<WorkflowPath, WorkflowVersionId>,
    ) -> ValidatedWorkflowVersion {
        ValidatedWorkflowVersion::new(
            WorkflowVersion::new(
                path("workflow.fabro"),
                BTreeMap::from([(path("workflow.fabro"), graph.to_owned())]),
                dependencies,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn version_id(version: &ValidatedWorkflowVersion) -> WorkflowVersionId {
        WorkflowVersionId::from(fabro_types::BlobHash::new(
            &version.version().canonical_bytes().unwrap(),
        ))
    }

    fn stores() -> (Arc<BlobStore>, WorkflowVersionStore) {
        let database = test_support::test_database();
        let blobs = database.blobs();
        let versions = WorkflowVersionStore::new(Arc::clone(&blobs));
        (blobs, versions)
    }

    #[tokio::test]
    async fn put_get_reuses_exact_blob_digest() {
        let (blobs, store) = stores();
        let version = version("digraph W {}", BTreeMap::new());
        let expected_bytes = version.version().canonical_bytes().unwrap();
        let expected_id = version_id(&version);

        let id = store.put(&version).await.unwrap();
        assert_eq!(id, expected_id);
        let blob_hash = id.into();
        assert_eq!(
            blobs.read(&blob_hash).await.unwrap().unwrap(),
            expected_bytes
        );
        assert_eq!(store.get(&id).await.unwrap(), Some(version));
    }

    #[tokio::test]
    async fn identical_content_is_idempotent() {
        let (_, store) = stores();
        let original = version("digraph W {}", BTreeMap::new());

        assert_eq!(
            store.put(&original).await.unwrap(),
            store.put(&original).await.unwrap()
        );

        let changed = version("digraph W { changed [label=\"yes\"] }", BTreeMap::new());
        assert_ne!(
            store.put(&original).await.unwrap(),
            store.put(&changed).await.unwrap()
        );
    }

    #[tokio::test]
    async fn dependency_must_be_stored_first() {
        let (blobs, store) = stores();
        let child = version("digraph Child {}", BTreeMap::new());
        let child_id = version_id(&child);
        let root = version(
            r#"digraph Root { child [stack.child_workflow="child.fabro"] }"#,
            BTreeMap::from([(path("child.fabro"), child_id)]),
        );
        let root_id = version_id(&root);

        let error = store.put(&root).await.unwrap_err();
        assert!(matches!(
            error,
            WorkflowVersionStoreError::DependencyNotFound { .. }
        ));
        assert!(!blobs.exists(&root_id.into()).await.unwrap());
        assert_eq!(store.put(&child).await.unwrap(), child_id);
        assert!(store.put(&root).await.is_ok());
    }

    #[tokio::test]
    async fn dependency_closure_must_be_complete_before_root_write() {
        let (blobs, store) = stores();
        let missing_grandchild_id = WorkflowVersionId::from(fabro_types::BlobHash::new(b"missing"));
        let child = version(
            r#"digraph Child { grandchild [stack.child_workflow="grandchild.fabro"] }"#,
            BTreeMap::from([(path("grandchild.fabro"), missing_grandchild_id)]),
        );
        let child_bytes = child.version().canonical_bytes().unwrap();
        let child_id = WorkflowVersionId::from(blobs.write(&child_bytes).await.unwrap());
        let root = version(
            r#"digraph Root { child [stack.child_workflow="child.fabro"] }"#,
            BTreeMap::from([(path("child.fabro"), child_id)]),
        );
        let root_id = version_id(&root);

        assert!(matches!(
            store.put(&root).await.unwrap_err(),
            WorkflowVersionStoreError::DependencyNotFound { id, .. }
                if id == missing_grandchild_id
        ));
        assert!(!blobs.exists(&root_id.into()).await.unwrap());
        assert!(matches!(
            store.get_closure(&child_id).await.unwrap_err(),
            WorkflowVersionStoreError::DependencyNotFound { id, .. }
                if id == missing_grandchild_id
        ));
    }

    #[tokio::test]
    async fn get_closure_returns_root_and_transitive_dependencies() {
        let (_, store) = stores();
        let grandchild = version("digraph Grandchild {}", BTreeMap::new());
        let grandchild_id = store.put(&grandchild).await.unwrap();
        let child = version(
            r#"digraph Child { grandchild [stack.child_workflow="grandchild.fabro"] }"#,
            BTreeMap::from([(path("grandchild.fabro"), grandchild_id)]),
        );
        let child_id = store.put(&child).await.unwrap();
        let root = version(
            r#"digraph Root { child [stack.child_workflow="child.fabro"] }"#,
            BTreeMap::from([(path("child.fabro"), child_id)]),
        );
        let root_id = store.put(&root).await.unwrap();

        let closure = store.get_closure(&root_id).await.unwrap().unwrap();

        assert_eq!(closure.root_id(), root_id);
        assert_eq!(closure.root(), root.version());
        assert_eq!(closure.get(&child_id), Some(child.version()));
        assert_eq!(closure.get(&grandchild_id), Some(grandchild.version()));
        assert_eq!(
            closure
                .versions()
                .map(|(id, version)| (id, version.clone()))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                (root_id, root.version().clone()),
                (child_id, child.version().clone()),
                (grandchild_id, grandchild.version().clone()),
            ])
        );
    }

    #[tokio::test]
    async fn get_closure_deduplicates_a_diamond() {
        let (_, store) = stores();
        let leaf = version("digraph Leaf {}", BTreeMap::new());
        let leaf_id = store.put(&leaf).await.unwrap();
        let left = version(
            r#"digraph Left { leaf [stack.child_workflow="leaf.fabro"] }"#,
            BTreeMap::from([(path("leaf.fabro"), leaf_id)]),
        );
        let left_id = store.put(&left).await.unwrap();
        let right = version(
            r#"digraph Right { leaf [stack.child_workflow="leaf.fabro"] }"#,
            BTreeMap::from([(path("leaf.fabro"), leaf_id)]),
        );
        let right_id = store.put(&right).await.unwrap();
        let root = version(
            r#"digraph Root {
                left [stack.child_workflow="left.fabro"]
                right [stack.child_workflow="right.fabro"]
            }"#,
            BTreeMap::from([
                (path("left.fabro"), left_id),
                (path("right.fabro"), right_id),
            ]),
        );
        let root_id = store.put(&root).await.unwrap();

        let closure = store.get_closure(&root_id).await.unwrap().unwrap();
        let ids = closure.versions().map(|(id, _)| id).collect::<Vec<_>>();

        assert_eq!(ids.len(), 4);
        assert_eq!(ids.iter().filter(|&&id| id == leaf_id).count(), 1);
    }

    #[tokio::test]
    async fn get_closure_preserves_noncanonical_dependency_errors() {
        let (blobs, store) = stores();
        let dependency = version("digraph Dependency {}", BTreeMap::new());
        let pretty = serde_json::to_vec_pretty(dependency.version()).unwrap();
        let dependency_id = WorkflowVersionId::from(blobs.write(&pretty).await.unwrap());
        let root = version(
            r#"digraph Root { dependency [stack.child_workflow="dependency.fabro"] }"#,
            BTreeMap::from([(path("dependency.fabro"), dependency_id)]),
        );
        let root_id = WorkflowVersionId::from(
            blobs
                .write(&root.version().canonical_bytes().unwrap())
                .await
                .unwrap(),
        );

        let error = store.get_closure(&root_id).await.unwrap_err();
        let WorkflowVersionStoreError::DependencyInvalid { source, .. } = error else {
            panic!("expected invalid dependency error");
        };
        assert!(matches!(
            source.as_ref(),
            WorkflowVersionStoreError::NonCanonical { id } if *id == dependency_id
        ));
        assert!(matches!(
            store.get_closure(&dependency_id).await.unwrap_err(),
            WorkflowVersionStoreError::NonCanonical { id } if id == dependency_id
        ));
    }

    #[tokio::test]
    async fn get_projects_the_same_validated_root_as_get_closure() {
        let (_, store) = stores();
        let child = version("digraph Child {}", BTreeMap::new());
        let child_id = store.put(&child).await.unwrap();
        let root = version(
            r#"digraph Root { child [stack.child_workflow="child.fabro"] }"#,
            BTreeMap::from([(path("child.fabro"), child_id)]),
        );
        let root_id = store.put(&root).await.unwrap();

        let closure = store.get_closure(&root_id).await.unwrap().unwrap();
        let projected = store.get(&root_id).await.unwrap().unwrap();

        assert_eq!(projected.version(), closure.root());
        let absent = version_id(&version("digraph Absent {}", BTreeMap::new()));
        assert!(store.get_closure(&absent).await.unwrap().is_none());
        assert!(store.get(&absent).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_rejects_arbitrary_and_noncanonical_blobs() {
        let (blobs, store) = stores();
        let arbitrary = WorkflowVersionId::from(blobs.write(b"not json").await.unwrap());
        assert!(matches!(
            store.get(&arbitrary).await.unwrap_err(),
            WorkflowVersionStoreError::Decode { .. }
        ));

        let invalid_bytes = br#"{"entrypoint":"missing.fabro","files":{"workflow.fabro":"digraph W {}"},"workflow_dependencies":{}}"#;
        let invalid = WorkflowVersionId::from(blobs.write(invalid_bytes).await.unwrap());
        assert!(matches!(
            store.get(&invalid).await.unwrap_err(),
            WorkflowVersionStoreError::Decode { .. }
        ));

        let version = version("digraph W {}", BTreeMap::new());
        let pretty = serde_json::to_vec_pretty(version.version()).unwrap();
        let noncanonical = WorkflowVersionId::from(blobs.write(&pretty).await.unwrap());
        assert!(matches!(
            store.get(&noncanonical).await.unwrap_err(),
            WorkflowVersionStoreError::NonCanonical { .. }
        ));
    }
}
