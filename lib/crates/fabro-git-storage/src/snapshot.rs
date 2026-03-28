use std::collections::BTreeMap;
use std::path::PathBuf;

use git2::{Oid, Signature};
use tracing::{debug, warn};

use crate::Result;
use crate::gitobj::{FileMode, Store, TreeEntries};

/// Options for writing a snapshot.
pub struct WriteOptions<'a> {
    pub branch: String,
    pub base_tree: Oid,
    pub changes: FileChanges,
    pub metadata: BTreeMap<String, Vec<u8>>,
    pub metadata_from_disk: Option<DiskDir>,
    pub author: Signature<'a>,
    pub message: String,
    pub deduplicate: bool,
}

/// File changes to apply from the working directory.
pub struct FileChanges {
    pub modified: Vec<String>,
    pub new: Vec<String>,
    pub deleted: Vec<String>,
    pub repo_root: PathBuf,
}

/// A directory on disk to walk and embed into the tree.
pub struct DiskDir {
    pub disk_path: PathBuf,
    pub tree_prefix: String,
}

/// Result of a snapshot write.
pub struct WriteResult {
    pub commit_oid: Oid,
    pub tree_oid: Oid,
    pub skipped: bool,
}

/// Metadata about a snapshot commit.
#[derive(Debug)]
pub struct SnapshotInfo {
    pub commit_oid: Oid,
    pub tree_oid: Oid,
    pub message: String,
    pub time: git2::Time,
}

/// Captures full repo-state on named branches.
pub struct SnapshotStore<'a> {
    objects: &'a Store,
}

impl<'a> SnapshotStore<'a> {
    pub fn new(objects: &'a Store) -> Self {
        Self { objects }
    }

    /// Write a snapshot to a branch.
    pub fn write(&self, opts: &WriteOptions<'_>) -> Result<WriteResult> {
        debug!(branch = %opts.branch, "Writing snapshot");
        // 1. Resolve existing branch tip or use base_tree
        let (base_tree_oid, parent_oid) = match self.objects.resolve_ref(&opts.branch)? {
            Some(commit_oid) => {
                let commit = self.objects.repo().find_commit(commit_oid)?;
                (commit.tree_id(), Some(commit_oid))
            }
            None => (opts.base_tree, None),
        };

        // 2. Flatten base tree
        let mut entries = self.objects.read_tree(base_tree_oid)?;

        // 3. Apply FileChanges
        for path in &opts.changes.deleted {
            entries.remove(path);
        }
        for path in opts.changes.modified.iter().chain(opts.changes.new.iter()) {
            let full_path = opts.changes.repo_root.join(path);
            match self.objects.write_blob_from_file(&full_path) {
                Ok((oid, mode)) => {
                    entries.set(path.clone(), oid, mode);
                }
                Err(crate::Error::ReadFile { .. }) => {
                    // File disappeared since detection — treat as deleted
                    warn!(path = %path, "File disappeared since detection, treating as deleted");
                    entries.remove(path);
                }
                Err(e) => return Err(e),
            }
        }

        // 4. Apply in-memory metadata
        for (path, content) in &opts.metadata {
            let oid = self.objects.write_blob(content)?;
            entries.set(path.clone(), oid, FileMode::Blob);
        }

        // 5. Walk metadata_from_disk
        if let Some(disk_dir) = &opts.metadata_from_disk {
            self.walk_disk_dir(&mut entries, disk_dir)?;
        }

        // 6. Write tree
        let new_tree_oid = self.objects.write_tree(&entries)?;

        // 7. Dedup check
        if opts.deduplicate {
            if let Some(parent) = parent_oid {
                let parent_commit = self.objects.repo().find_commit(parent)?;
                if parent_commit.tree_id() == new_tree_oid {
                    debug!(branch = %opts.branch, "Snapshot skipped (tree unchanged)");
                    return Ok(WriteResult {
                        commit_oid: parent,
                        tree_oid: new_tree_oid,
                        skipped: true,
                    });
                }
            }
        }

        // 8. Create commit
        let parents: Vec<Oid> = parent_oid.into_iter().collect();
        let commit_oid =
            self.objects
                .write_commit(new_tree_oid, &parents, &opts.message, &opts.author)?;

        // 9. Update ref
        self.objects.update_ref(&opts.branch, commit_oid)?;
        debug!(branch = %opts.branch, commit = %commit_oid, "Snapshot written");

        Ok(WriteResult {
            commit_oid,
            tree_oid: new_tree_oid,
            skipped: false,
        })
    }

    /// Tip commit of a snapshot branch. `None` if branch doesn't exist.
    pub fn latest(&self, branch: &str) -> Result<Option<SnapshotInfo>> {
        let commit_oid = match self.objects.resolve_ref(branch)? {
            Some(oid) => oid,
            None => return Ok(None),
        };
        let commit = self.objects.repo().find_commit(commit_oid)?;
        let tree_oid = commit.tree_id();
        let message = commit.message().unwrap_or("").to_string();
        let time = commit.author().when();
        Ok(Some(SnapshotInfo {
            commit_oid,
            tree_oid,
            message,
            time,
        }))
    }

    /// Read a single file from a snapshot commit's tree.
    pub fn read_file(&self, commit_oid: Oid, path: &str) -> Result<Option<Vec<u8>>> {
        let commit = self.objects.repo().find_commit(commit_oid)?;
        let tree = commit.tree()?;
        match tree.get_path(std::path::Path::new(path)) {
            Ok(entry) => {
                let blob = self.objects.repo().find_blob(entry.id())?;
                Ok(Some(blob.content().to_vec()))
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Walk commits on a snapshot branch, newest first.
    pub fn list_commits(&self, branch: &str, limit: usize) -> Result<Vec<SnapshotInfo>> {
        let commit_oid = match self.objects.resolve_ref(branch)? {
            Some(oid) => oid,
            None => return Ok(vec![]),
        };
        let mut revwalk = self.objects.repo().revwalk()?;
        revwalk.set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)?;
        revwalk.push(commit_oid)?;

        let mut results = Vec::new();
        for oid_result in revwalk.take(limit) {
            let oid = oid_result?;
            let commit = self.objects.repo().find_commit(oid)?;
            results.push(SnapshotInfo {
                commit_oid: oid,
                tree_oid: commit.tree_id(),
                message: commit.message().unwrap_or("").to_string(),
                time: commit.author().when(),
            });
        }
        Ok(results)
    }

    /// Check if a snapshot branch exists.
    pub fn exists(&self, branch: &str) -> Result<bool> {
        Ok(self.objects.resolve_ref(branch)?.is_some())
    }

    /// Delete a snapshot branch.
    pub fn delete(&self, branch: &str) -> Result<()> {
        debug!(branch = %branch, "Deleting snapshot branch");
        self.objects.delete_ref(branch)
    }

    /// Rename a snapshot branch.
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        debug!(old = %old, new = %new, "Renaming snapshot branch");
        let oid = self
            .objects
            .resolve_ref(old)?
            .ok_or_else(|| crate::Error::BranchNotFound {
                branch: old.to_string(),
            })?;
        self.objects.update_ref(new, oid)?;
        self.objects.delete_ref(old)?;
        Ok(())
    }

    /// List snapshot branches matching a prefix.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let full_prefix = format!("refs/heads/{prefix}");
        let mut branches = Vec::new();
        for reference in self
            .objects
            .repo()
            .references_glob(&format!("{full_prefix}*"))?
        {
            let reference = reference?;
            if let Some(name) = reference.name() {
                if let Some(branch) = name.strip_prefix("refs/heads/") {
                    branches.push(branch.to_string());
                }
            }
        }
        branches.sort();
        Ok(branches)
    }

    /// Walk a directory on disk and add files to tree entries.
    fn walk_disk_dir(&self, entries: &mut TreeEntries, disk_dir: &DiskDir) -> Result<()> {
        let walker = walkdir::WalkDir::new(&disk_dir.disk_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok());

        for entry in walker {
            // Skip symlinks
            if entry.path_is_symlink() {
                continue;
            }
            // Skip directories
            if entry.file_type().is_dir() {
                continue;
            }

            let relative = entry
                .path()
                .strip_prefix(&disk_dir.disk_path)
                .unwrap_or(entry.path());
            let tree_path = if disk_dir.tree_prefix.is_empty() {
                relative.to_string_lossy().to_string()
            } else {
                format!(
                    "{}/{}",
                    disk_dir.tree_prefix.trim_end_matches('/'),
                    relative.to_string_lossy()
                )
            };

            let (oid, mode) = self.objects.write_blob_from_file(entry.path())?;
            entries.set(tree_path, oid, mode);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;

    fn temp_repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, Store::new(repo))
    }

    fn test_sig() -> Signature<'static> {
        Signature::now("Test", "test@example.com").unwrap()
    }

    fn empty_changes() -> FileChanges {
        FileChanges {
            modified: vec![],
            new: vec![],
            deleted: vec![],
            repo_root: PathBuf::from("/tmp"),
        }
    }

    // -- write creates branch + commit from base tree --

    #[test]
    fn write_creates_branch_from_base_tree() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let snap = SnapshotStore::new(&store);

        // Create a base tree with one file
        let blob_oid = store.write_blob(b"base content").unwrap();
        let mut base_entries = TreeEntries::new();
        base_entries.set("existing.txt", blob_oid, FileMode::Blob);
        let base_tree = store.write_tree(&base_entries).unwrap();

        let result = snap
            .write(&WriteOptions {
                branch: "snap/test".to_string(),
                base_tree,
                changes: empty_changes(),
                metadata: BTreeMap::new(),
                metadata_from_disk: None,
                author: sig,
                message: "snapshot 1".to_string(),
                deduplicate: false,
            })
            .unwrap();

        assert!(!result.skipped);
        assert!(store.resolve_ref("snap/test").unwrap().is_some());

        // Verify the file is in the snapshot
        let content = snap.read_file(result.commit_oid, "existing.txt").unwrap();
        assert_eq!(content.unwrap(), b"base content");
    }

    // -- write applies file changes --

    #[test]
    fn write_applies_file_changes() {
        let (dir, store) = temp_repo();
        let sig = test_sig();
        let snap = SnapshotStore::new(&store);

        // Create files on disk in the repo root
        let repo_root = dir.path().to_path_buf();
        std::fs::write(repo_root.join("new_file.txt"), b"new content").unwrap();
        std::fs::write(repo_root.join("modified.txt"), b"modified content").unwrap();

        // Create base tree with a file to delete and one to modify
        let old_blob = store.write_blob(b"old content").unwrap();
        let delete_blob = store.write_blob(b"delete me").unwrap();
        let mut base_entries = TreeEntries::new();
        base_entries.set("modified.txt", old_blob, FileMode::Blob);
        base_entries.set("to_delete.txt", delete_blob, FileMode::Blob);
        let base_tree = store.write_tree(&base_entries).unwrap();

        let result = snap
            .write(&WriteOptions {
                branch: "snap/changes".to_string(),
                base_tree,
                changes: FileChanges {
                    modified: vec!["modified.txt".to_string()],
                    new: vec!["new_file.txt".to_string()],
                    deleted: vec!["to_delete.txt".to_string()],
                    repo_root,
                },
                metadata: BTreeMap::new(),
                metadata_from_disk: None,
                author: sig,
                message: "apply changes".to_string(),
                deduplicate: false,
            })
            .unwrap();

        assert_eq!(
            snap.read_file(result.commit_oid, "modified.txt")
                .unwrap()
                .unwrap(),
            b"modified content"
        );
        assert_eq!(
            snap.read_file(result.commit_oid, "new_file.txt")
                .unwrap()
                .unwrap(),
            b"new content"
        );
        assert!(
            snap.read_file(result.commit_oid, "to_delete.txt")
                .unwrap()
                .is_none()
        );
    }

    // -- write embeds in-memory metadata --

    #[test]
    fn write_embeds_metadata() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();

        let mut metadata = BTreeMap::new();
        metadata.insert(
            ".meta/transcript.jsonl".to_string(),
            b"line1\nline2".to_vec(),
        );

        let result = snap
            .write(&WriteOptions {
                branch: "snap/meta".to_string(),
                base_tree,
                changes: empty_changes(),
                metadata,
                metadata_from_disk: None,
                author: sig,
                message: "with metadata".to_string(),
                deduplicate: false,
            })
            .unwrap();

        let content = snap
            .read_file(result.commit_oid, ".meta/transcript.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(content, b"line1\nline2");
    }

    // -- write dedup skips when tree unchanged --

    #[test]
    fn write_dedup_skips_unchanged() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();
        let sig = test_sig();

        // First write
        let result1 = snap
            .write(&WriteOptions {
                branch: "snap/dedup".to_string(),
                base_tree,
                changes: empty_changes(),
                metadata: BTreeMap::new(),
                metadata_from_disk: None,
                author: sig.clone(),
                message: "first".to_string(),
                deduplicate: true,
            })
            .unwrap();
        assert!(!result1.skipped);

        // Second write with same content — should be skipped
        let sig2 = test_sig();
        let result2 = snap
            .write(&WriteOptions {
                branch: "snap/dedup".to_string(),
                base_tree,
                changes: empty_changes(),
                metadata: BTreeMap::new(),
                metadata_from_disk: None,
                author: sig2,
                message: "second".to_string(),
                deduplicate: true,
            })
            .unwrap();
        assert!(result2.skipped);
        assert_eq!(result2.commit_oid, result1.commit_oid);
    }

    // -- latest / read_file / list_commits --

    #[test]
    fn latest_and_list_commits() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();

        // Write two snapshots
        let sig1 = test_sig();
        snap.write(&WriteOptions {
            branch: "snap/history".to_string(),
            base_tree,
            changes: empty_changes(),
            metadata: BTreeMap::from([("a.txt".to_string(), b"a".to_vec())]),
            metadata_from_disk: None,
            author: sig1,
            message: "first".to_string(),
            deduplicate: false,
        })
        .unwrap();

        let sig2 = test_sig();
        snap.write(&WriteOptions {
            branch: "snap/history".to_string(),
            base_tree,
            changes: empty_changes(),
            metadata: BTreeMap::from([("b.txt".to_string(), b"b".to_vec())]),
            metadata_from_disk: None,
            author: sig2,
            message: "second".to_string(),
            deduplicate: false,
        })
        .unwrap();

        let latest = snap.latest("snap/history").unwrap().unwrap();
        assert_eq!(latest.message, "second");

        let commits = snap.list_commits("snap/history", 10).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].message, "second");
        assert_eq!(commits[1].message, "first");
    }

    #[test]
    fn latest_nonexistent() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        assert!(snap.latest("nonexistent").unwrap().is_none());
    }

    // -- exists / delete / rename / list --

    #[test]
    fn exists_and_delete() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();
        let sig = test_sig();

        snap.write(&WriteOptions {
            branch: "snap/del".to_string(),
            base_tree,
            changes: empty_changes(),
            metadata: BTreeMap::new(),
            metadata_from_disk: None,
            author: sig,
            message: "create".to_string(),
            deduplicate: false,
        })
        .unwrap();

        assert!(snap.exists("snap/del").unwrap());
        snap.delete("snap/del").unwrap();
        assert!(!snap.exists("snap/del").unwrap());
    }

    #[test]
    fn rename_branch() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();
        let sig = test_sig();

        snap.write(&WriteOptions {
            branch: "snap/old".to_string(),
            base_tree,
            changes: empty_changes(),
            metadata: BTreeMap::from([("file.txt".to_string(), b"data".to_vec())]),
            metadata_from_disk: None,
            author: sig,
            message: "create".to_string(),
            deduplicate: false,
        })
        .unwrap();

        snap.rename("snap/old", "snap/new").unwrap();
        assert!(!snap.exists("snap/old").unwrap());
        assert!(snap.exists("snap/new").unwrap());

        // Verify data is preserved
        let info = snap.latest("snap/new").unwrap().unwrap();
        let content = snap.read_file(info.commit_oid, "file.txt").unwrap();
        assert_eq!(content.unwrap(), b"data");
    }

    #[test]
    fn list_branches() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();

        // Create several branches
        for name in &["snap/a", "snap/b", "other/c"] {
            let sig = test_sig();
            snap.write(&WriteOptions {
                branch: name.to_string(),
                base_tree,
                changes: empty_changes(),
                metadata: BTreeMap::new(),
                metadata_from_disk: None,
                author: sig,
                message: "create".to_string(),
                deduplicate: false,
            })
            .unwrap();
        }

        let snap_branches = snap.list("snap/").unwrap();
        assert_eq!(snap_branches, vec!["snap/a", "snap/b"]);
    }

    // -- metadata_from_disk --

    #[test]
    fn write_metadata_from_disk() {
        let (_dir, store) = temp_repo();
        let snap = SnapshotStore::new(&store);
        let base_tree = store.write_empty_tree().unwrap();

        // Create a temp directory with files
        let meta_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(meta_dir.path().join("info.json"), b"{}").unwrap();
        std::fs::create_dir(meta_dir.path().join("sub")).unwrap();
        std::fs::write(meta_dir.path().join("sub/data.txt"), b"nested").unwrap();

        let sig = test_sig();
        let result = snap
            .write(&WriteOptions {
                branch: "snap/disk".to_string(),
                base_tree,
                changes: empty_changes(),
                metadata: BTreeMap::new(),
                metadata_from_disk: Some(DiskDir {
                    disk_path: meta_dir.path().to_path_buf(),
                    tree_prefix: ".meta".to_string(),
                }),
                author: sig,
                message: "from disk".to_string(),
                deduplicate: false,
            })
            .unwrap();

        assert_eq!(
            snap.read_file(result.commit_oid, ".meta/info.json")
                .unwrap()
                .unwrap(),
            b"{}"
        );
        assert_eq!(
            snap.read_file(result.commit_oid, ".meta/sub/data.txt")
                .unwrap()
                .unwrap(),
            b"nested"
        );
    }
}
