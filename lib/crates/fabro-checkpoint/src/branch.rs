use git2::{Oid, Signature};
use tracing::{debug, warn};

use crate::Result;
use crate::git::{FileMode, Store, TreeEntries};

/// Metadata about a commit, returned by `log`.
#[derive(Debug)]
pub struct CommitInfo {
    pub oid: Oid,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub time: git2::Time,
}

/// Key-value storage on a single git branch. Each write creates one commit.
/// The branch's tree grows monotonically — each commit's tree is a superset of the previous.
pub struct BranchStore<'a> {
    objects: &'a Store,
    branch: String,
    author: Signature<'static>,
}

impl<'a> BranchStore<'a> {
    pub fn new(objects: &'a Store, branch: impl Into<String>, author: &Signature<'_>) -> Self {
        // Clone to 'static by using Signature::now (author name/email are copied into owned strings)
        let author_static = Signature::now(
            author.name().unwrap_or("unknown"),
            author.email().unwrap_or(""),
        )
        .expect("creating signature should not fail");
        Self {
            objects,
            branch: branch.into(),
            author: author_static,
        }
    }

    /// Create branch with empty root commit if it doesn't exist.
    pub fn ensure_branch(&self) -> Result<()> {
        if self.objects.resolve_ref(&self.branch)?.is_some() {
            return Ok(());
        }
        let empty_tree = self.objects.write_empty_tree()?;
        let commit_oid =
            self.objects
                .write_commit(empty_tree, &[], "initialize branch", &self.author)?;
        self.objects.update_ref(&self.branch, commit_oid)?;
        debug!(branch = %self.branch, "Created git storage branch");
        Ok(())
    }

    /// Core read-modify-write: read current tree, let caller mutate, write new commit.
    pub fn write_with(
        &self,
        message: &str,
        f: impl FnOnce(&mut TreeEntries) -> Result<()>,
    ) -> Result<Oid> {
        let parent_oid = self.objects.resolve_ref(&self.branch)?.ok_or_else(|| {
            warn!(branch = %self.branch, "Branch not found during write");
            crate::Error::BranchNotFound {
                branch: self.branch.clone(),
            }
        })?;
        let parent_commit = self.objects.repo().find_commit(parent_oid)?;
        let tree_oid = parent_commit.tree_id();
        let mut entries = self.objects.read_tree(tree_oid)?;

        f(&mut entries)?;

        let new_tree = self.objects.write_tree(&entries)?;
        let commit_oid =
            self.objects
                .write_commit(new_tree, &[parent_oid], message, &self.author)?;
        self.objects.update_ref(&self.branch, commit_oid)?;
        debug!(branch = %self.branch, commit = %commit_oid, "Wrote git storage commit");
        Ok(commit_oid)
    }

    /// Store a single file. Creates one commit.
    pub fn write_entry(&self, path: &str, content: &[u8], message: &str) -> Result<Oid> {
        let blob_oid = self.objects.write_blob(content)?;
        self.write_with(message, |entries| {
            entries.set(path, blob_oid, FileMode::Blob);
            Ok(())
        })
    }

    /// Atomically store multiple files in a single commit.
    pub fn write_entries(&self, file_entries: &[(&str, &[u8])], message: &str) -> Result<Oid> {
        let blobs: Vec<(String, Oid)> = file_entries
            .iter()
            .map(|(path, content)| {
                let oid = self.objects.write_blob(content)?;
                Ok((path.to_string(), oid))
            })
            .collect::<Result<Vec<_>>>()?;

        self.write_with(message, |entries| {
            for (path, oid) in &blobs {
                entries.set(path.clone(), *oid, FileMode::Blob);
            }
            Ok(())
        })
    }

    /// Remove a file. Creates one commit.
    pub fn delete_entry(&self, path: &str, message: &str) -> Result<Oid> {
        self.write_with(message, |entries| {
            entries.remove(path);
            Ok(())
        })
    }

    /// Read a single file from the latest tree. Returns `None` if branch or path doesn't exist.
    pub fn read_entry(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let Some(commit_oid) = self.objects.resolve_ref(&self.branch)? else {
            return Ok(None);
        };
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

    /// Read multiple paths. Missing paths are omitted from the result.
    pub fn read_entries<'b>(&self, paths: &[&'b str]) -> Result<Vec<(&'b str, Vec<u8>)>> {
        let Some(commit_oid) = self.objects.resolve_ref(&self.branch)? else {
            return Ok(vec![]);
        };
        let commit = self.objects.repo().find_commit(commit_oid)?;
        let tree = commit.tree()?;

        let mut results = Vec::new();
        for path in paths {
            match tree.get_path(std::path::Path::new(path)) {
                Ok(entry) => {
                    let blob = self.objects.repo().find_blob(entry.id())?;
                    results.push((*path, blob.content().to_vec()));
                }
                Err(e) if e.code() == git2::ErrorCode::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(results)
    }

    /// List all paths under a prefix in the latest tree.
    pub fn list_entries(&self, prefix: &str) -> Result<Vec<String>> {
        let Some(commit_oid) = self.objects.resolve_ref(&self.branch)? else {
            return Ok(vec![]);
        };
        let commit = self.objects.repo().find_commit(commit_oid)?;
        let tree_oid = commit.tree_id();
        let entries = self.objects.read_tree(tree_oid)?;
        let paths: Vec<String> = entries
            .under_prefix(prefix)
            .map(|(k, _)| k.to_string())
            .collect();
        Ok(paths)
    }

    /// Full tree from branch tip.
    pub fn tip_tree(&self) -> Result<TreeEntries> {
        let commit_oid = self.objects.resolve_ref(&self.branch)?.ok_or_else(|| {
            crate::Error::BranchNotFound {
                branch: self.branch.clone(),
            }
        })?;
        let commit = self.objects.repo().find_commit(commit_oid)?;
        self.objects.read_tree(commit.tree_id())
    }

    /// Walk commits on the branch, newest first.
    pub fn log(&self, limit: usize) -> Result<Vec<CommitInfo>> {
        let Some(commit_oid) = self.objects.resolve_ref(&self.branch)? else {
            return Ok(vec![]);
        };
        let mut revwalk = self.objects.repo().revwalk()?;
        revwalk.set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)?;
        revwalk.push(commit_oid)?;

        let mut results = Vec::new();
        for oid_result in revwalk.take(limit) {
            let oid = oid_result?;
            let commit = self.objects.repo().find_commit(oid)?;
            results.push(CommitInfo {
                oid,
                message: commit.message().unwrap_or("").to_string(),
                author_name: commit.author().name().unwrap_or("").to_string(),
                author_email: commit.author().email().unwrap_or("").to_string(),
                time: commit.author().when(),
            });
        }
        Ok(results)
    }
}

/// Split a hex ID into a sharded path.
///
/// ```text
/// sharded_path("a3b2c4d5e6f7", 2) → "a3/b2c4d5e6f7"
/// ```
pub fn sharded_path(id: &str, prefix_len: usize) -> String {
    if id.len() <= prefix_len {
        return id.to_string();
    }
    format!("{}/{}", &id[..prefix_len], &id[prefix_len..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileMode;
    use git2::Repository;

    fn temp_repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, Store::new(repo))
    }

    fn test_sig() -> Signature<'static> {
        Signature::now("Test", "test@example.com").unwrap()
    }

    // -- sharded_path (pure function) --

    #[test]
    fn sharded_path_basic() {
        assert_eq!(sharded_path("a3b2c4d5e6f7", 2), "a3/b2c4d5e6f7");
    }

    #[test]
    fn sharded_path_short_id() {
        assert_eq!(sharded_path("ab", 2), "ab");
    }

    #[test]
    fn sharded_path_prefix_3() {
        assert_eq!(sharded_path("abcdef", 3), "abc/def");
    }

    // -- ensure_branch --

    #[test]
    fn ensure_branch_creates_branch() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/metadata", &sig);
        bs.ensure_branch().unwrap();
        assert!(store.resolve_ref("test/metadata").unwrap().is_some());
    }

    #[test]
    fn ensure_branch_idempotent() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/metadata", &sig);
        bs.ensure_branch().unwrap();
        let first_oid = store.resolve_ref("test/metadata").unwrap().unwrap();
        bs.ensure_branch().unwrap();
        let second_oid = store.resolve_ref("test/metadata").unwrap().unwrap();
        assert_eq!(first_oid, second_oid);
    }

    // -- write_entry + read_entry roundtrip --

    #[test]
    fn write_and_read_entry() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("hello.txt", b"world", "add hello").unwrap();
        let content = bs.read_entry("hello.txt").unwrap().unwrap();
        assert_eq!(content, b"world");
    }

    // -- write_entries atomic multi-file --

    #[test]
    fn write_entries_atomic() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entries(&[("a.txt", b"alpha"), ("b.txt", b"beta")], "add both")
            .unwrap();

        assert_eq!(bs.read_entry("a.txt").unwrap().unwrap(), b"alpha");
        assert_eq!(bs.read_entry("b.txt").unwrap().unwrap(), b"beta");

        // Verify it was a single commit (2 total: init + write)
        let log = bs.log(10).unwrap();
        assert_eq!(log.len(), 2);
    }

    // -- delete_entry --

    #[test]
    fn delete_entry() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("to-delete.txt", b"content", "add file")
            .unwrap();
        bs.delete_entry("to-delete.txt", "remove file").unwrap();
        assert!(bs.read_entry("to-delete.txt").unwrap().is_none());
    }

    // -- write_with closure --

    #[test]
    fn write_with_closure() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        let blob_oid = store.write_blob(b"custom content").unwrap();
        bs.write_with("custom write", |entries| {
            entries.set("custom.txt", blob_oid, FileMode::Blob);
            Ok(())
        })
        .unwrap();

        let content = bs.read_entry("custom.txt").unwrap().unwrap();
        assert_eq!(content, b"custom content");
    }

    // -- read_entry on nonexistent --

    #[test]
    fn read_entry_nonexistent_branch() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "nonexistent", &sig);
        assert!(bs.read_entry("anything.txt").unwrap().is_none());
    }

    #[test]
    fn read_entry_nonexistent_path() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();
        assert!(bs.read_entry("nonexistent.txt").unwrap().is_none());
    }

    // -- list_entries --

    #[test]
    fn list_entries_with_prefix() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entries(
            &[
                ("ab/data.json", b"{}"),
                ("ab/meta.json", b"{}"),
                ("cd/other.json", b"{}"),
            ],
            "add files",
        )
        .unwrap();

        let ab_entries = bs.list_entries("ab/").unwrap();
        assert_eq!(ab_entries, vec!["ab/data.json", "ab/meta.json"]);
    }

    // -- tip_tree --

    #[test]
    fn tip_tree() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("file.txt", b"content", "add file").unwrap();
        let tree = bs.tip_tree().unwrap();
        assert_eq!(tree.len(), 1);
        assert!(tree.get("file.txt").is_some());
    }

    // -- log --

    #[test]
    fn log_returns_history() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("a.txt", b"a", "first write").unwrap();
        bs.write_entry("b.txt", b"b", "second write").unwrap();

        let log = bs.log(10).unwrap();
        assert_eq!(log.len(), 3); // init + 2 writes
        assert_eq!(log[0].message, "second write");
        assert_eq!(log[1].message, "first write");
        assert_eq!(log[2].message, "initialize branch");
    }

    #[test]
    fn log_respects_limit() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entry("a.txt", b"a", "first").unwrap();
        bs.write_entry("b.txt", b"b", "second").unwrap();

        let log = bs.log(2).unwrap();
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn log_empty_branch() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "nonexistent", &sig);
        let log = bs.log(10).unwrap();
        assert!(log.is_empty());
    }

    // -- read_entries --

    #[test]
    fn read_entries_multiple() {
        let (_dir, store) = temp_repo();
        let sig = test_sig();
        let bs = BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();

        bs.write_entries(&[("a.txt", b"alpha"), ("b.txt", b"beta")], "add files")
            .unwrap();

        let results = bs.read_entries(&["a.txt", "b.txt", "missing.txt"]).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], ("a.txt", b"alpha".to_vec()));
        assert_eq!(results[1], ("b.txt", b"beta".to_vec()));
    }
}
