use std::collections::BTreeMap;
use std::path::Path;

use git2::{Oid, Repository, Signature};

use crate::{Error, Result};

/// Git file modes for tree entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    Blob,
    BlobExecutable,
    Tree,
}

impl FileMode {
    fn as_i32(self) -> i32 {
        match self {
            Self::Blob => 0o100644,
            Self::BlobExecutable => 0o100755,
            Self::Tree => 0o040000,
        }
    }

    fn from_i32(mode: i32) -> Self {
        match mode {
            0o100755 => Self::BlobExecutable,
            0o040000 => Self::Tree,
            _ => Self::Blob,
        }
    }
}

/// A single entry in a flat tree map.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub oid: Oid,
    pub filemode: FileMode,
}

/// A flat, sorted map of paths to tree entries.
///
/// Intermediate representation between reading an existing git tree and writing a new one.
/// Paths use forward slashes and are relative to the tree root (e.g. `"src/main.rs"`).
#[derive(Debug, Clone, Default)]
pub struct TreeEntries(BTreeMap<String, TreeEntry>);

impl TreeEntries {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn set(&mut self, path: impl Into<String>, oid: Oid, filemode: FileMode) {
        self.0.insert(path.into(), TreeEntry { oid, filemode });
    }

    pub fn remove(&mut self, path: &str) {
        self.0.remove(path);
    }

    pub fn get(&self, path: &str) -> Option<&TreeEntry> {
        self.0.get(path)
    }

    pub fn merge(&mut self, other: &Self) {
        for (path, entry) in &other.0 {
            self.0.insert(path.clone(), entry.clone());
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &TreeEntry)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Iterate entries whose paths start with the given prefix.
    pub fn under_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a TreeEntry)> {
        self.0
            .range(prefix.to_string()..)
            .take_while(move |(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Wraps a `git2::Repository` with operations for creating blobs, trees, commits, and refs.
pub struct Store {
    repo: Repository,
}

impl Store {
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    pub fn repo_dir(&self) -> &Path {
        self.repo
            .workdir()
            .or_else(|| self.repo.path().parent())
            .unwrap_or(self.repo.path())
    }

    /// Store bytes as a git blob.
    pub fn write_blob(&self, content: &[u8]) -> Result<Oid> {
        Ok(self.repo.blob(content)?)
    }

    /// Read a file from disk, store as a blob.
    /// Returns `(oid, filemode)` where filemode detects the executable bit on unix.
    pub fn write_blob_from_file(&self, path: &Path) -> Result<(Oid, FileMode)> {
        let content = std::fs::read(path).map_err(|e| Error::ReadFile {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mode = detect_filemode(path);
        let oid = self.repo.blob(&content)?;
        Ok((oid, mode))
    }

    /// Recursively flatten a git tree into `TreeEntries`.
    pub fn read_tree(&self, oid: Oid) -> Result<TreeEntries> {
        let tree = self.repo.find_tree(oid)?;
        let mut entries = TreeEntries::new();
        read_tree_recursive(&self.repo, &tree, "", &mut entries)?;
        Ok(entries)
    }

    /// Build nested git tree objects from flat `TreeEntries`.
    pub fn write_tree(&self, entries: &TreeEntries) -> Result<Oid> {
        let root = build_dir_node(entries);
        write_dir_node(&self.repo, &root)
    }

    /// Write an empty tree (zero entries).
    pub fn write_empty_tree(&self) -> Result<Oid> {
        let builder = self.repo.treebuilder(None)?;
        Ok(builder.write()?)
    }

    /// Create a commit. Does NOT update any ref — caller does that via `update_ref`.
    /// `author` is used for both author and committer fields.
    pub fn write_commit(
        &self,
        tree_oid: Oid,
        parents: &[Oid],
        message: &str,
        author: &Signature<'_>,
    ) -> Result<Oid> {
        let tree = self.repo.find_tree(tree_oid)?;
        let parent_commits: Vec<git2::Commit<'_>> = parents
            .iter()
            .map(|oid| self.repo.find_commit(*oid))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parent_refs: Vec<&git2::Commit<'_>> = parent_commits.iter().collect();
        let oid = self
            .repo
            .commit(None, author, author, message, &tree, &parent_refs)?;
        Ok(oid)
    }

    /// Set a branch ref to point at `commit_oid`. Creates branch if needed.
    pub fn update_ref(&self, branch: &str, commit_oid: Oid) -> Result<()> {
        let refname = format!("refs/heads/{branch}");
        self.repo
            .reference(&refname, commit_oid, true, "update ref")?;
        Ok(())
    }

    /// Resolve branch to commit OID. Returns `None` if branch doesn't exist.
    pub fn resolve_ref(&self, branch: &str) -> Result<Option<Oid>> {
        let refname = format!("refs/heads/{branch}");
        match self.repo.find_reference(&refname) {
            Ok(reference) => Ok(Some(reference.peel_to_commit()?.id())),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Read a blob from the tree of a specific commit. Returns `None` if the path doesn't exist.
    pub fn read_blob_at(&self, commit_oid: Oid, path: &str) -> Result<Option<Vec<u8>>> {
        let commit = self.repo.find_commit(commit_oid)?;
        let tree = commit.tree()?;
        match tree.get_path(std::path::Path::new(path)) {
            Ok(entry) => {
                let blob = self.repo.find_blob(entry.id())?;
                Ok(Some(blob.content().to_vec()))
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a branch reference. No-op if branch doesn't exist.
    pub fn delete_ref(&self, branch: &str) -> Result<()> {
        let refname = format!("refs/heads/{branch}");
        match self.repo.find_reference(&refname) {
            Ok(mut reference) => {
                reference.delete()?;
                Ok(())
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Recursively read a git tree into flat `TreeEntries`.
fn read_tree_recursive(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    entries: &mut TreeEntries,
) -> Result<()> {
    for entry in tree {
        let name = entry.name().unwrap_or("");
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };

        let mode = FileMode::from_i32(entry.filemode());
        if mode == FileMode::Tree {
            let subtree = repo.find_tree(entry.id())?;
            read_tree_recursive(repo, &subtree, &path, entries)?;
        } else {
            entries.set(path, entry.id(), mode);
        }
    }
    Ok(())
}

/// Intermediate structure for building nested git trees from flat paths.
struct DirNode {
    files: BTreeMap<String, TreeEntry>,
    dirs: BTreeMap<String, Self>,
}

impl DirNode {
    fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            dirs: BTreeMap::new(),
        }
    }
}

/// Build a `DirNode` tree from flat `TreeEntries`.
fn build_dir_node(entries: &TreeEntries) -> DirNode {
    let mut root = DirNode::new();
    for (path, entry) in entries.iter() {
        let parts: Vec<&str> = path.split('/').collect();
        insert_into_dir_node(&mut root, &parts, entry);
    }
    root
}

fn insert_into_dir_node(node: &mut DirNode, parts: &[&str], entry: &TreeEntry) {
    match parts {
        [name] => {
            node.files.insert(name.to_string(), entry.clone());
        }
        [dir, rest @ ..] => {
            let child = node
                .dirs
                .entry(dir.to_string())
                .or_insert_with(DirNode::new);
            insert_into_dir_node(child, rest, entry);
        }
        [] => {}
    }
}

/// Recursively write a `DirNode` as nested git trees, bottom-up.
fn write_dir_node(repo: &Repository, node: &DirNode) -> Result<Oid> {
    let mut builder = repo.treebuilder(None)?;

    for (name, entry) in &node.files {
        builder.insert(name, entry.oid, entry.filemode.as_i32())?;
    }

    for (name, child) in &node.dirs {
        let child_oid = write_dir_node(repo, child)?;
        builder.insert(name, child_oid, FileMode::Tree.as_i32())?;
    }

    Ok(builder.write()?)
}

/// Detect file mode (executable or not) from filesystem metadata.
#[cfg(unix)]
fn detect_filemode(path: &Path) -> FileMode {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => {
            if meta.permissions().mode() & 0o111 != 0 {
                FileMode::BlobExecutable
            } else {
                FileMode::Blob
            }
        }
        Err(_) => FileMode::Blob,
    }
}

#[cfg(not(unix))]
fn detect_filemode(_path: &Path) -> FileMode {
    FileMode::Blob
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, Store::new(repo))
    }

    // -- TreeEntries tests (pure data, no git) --

    #[test]
    fn tree_entries_set_and_get() {
        let mut entries = TreeEntries::new();
        let oid = Oid::zero();
        entries.set("src/main.rs", oid, FileMode::Blob);
        let entry = entries.get("src/main.rs").unwrap();
        assert_eq!(entry.oid, oid);
        assert_eq!(entry.filemode, FileMode::Blob);
    }

    #[test]
    fn tree_entries_remove() {
        let mut entries = TreeEntries::new();
        entries.set("a.txt", Oid::zero(), FileMode::Blob);
        entries.set("b.txt", Oid::zero(), FileMode::Blob);
        entries.remove("a.txt");
        assert!(entries.get("a.txt").is_none());
        assert!(entries.get("b.txt").is_some());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn tree_entries_merge() {
        let mut a = TreeEntries::new();
        a.set("file1.txt", Oid::zero(), FileMode::Blob);

        let mut b = TreeEntries::new();
        b.set("file2.txt", Oid::zero(), FileMode::Blob);

        a.merge(&b);
        assert_eq!(a.len(), 2);
        assert!(a.get("file1.txt").is_some());
        assert!(a.get("file2.txt").is_some());
    }

    #[test]
    fn tree_entries_under_prefix() {
        let mut entries = TreeEntries::new();
        entries.set("src/a.rs", Oid::zero(), FileMode::Blob);
        entries.set("src/b.rs", Oid::zero(), FileMode::Blob);
        entries.set("test/c.rs", Oid::zero(), FileMode::Blob);

        let src: Vec<&str> = entries.under_prefix("src/").map(|(k, _)| k).collect();
        assert_eq!(src, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn tree_entries_empty() {
        let entries = TreeEntries::new();
        assert!(entries.is_empty());
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn tree_entries_iter_sorted() {
        let mut entries = TreeEntries::new();
        entries.set("z.txt", Oid::zero(), FileMode::Blob);
        entries.set("a.txt", Oid::zero(), FileMode::Blob);
        entries.set("m.txt", Oid::zero(), FileMode::Blob);

        let keys: Vec<&str> = entries.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["a.txt", "m.txt", "z.txt"]);
    }

    // -- write_blob / read back --

    #[test]
    fn write_blob_and_read_back() {
        let (_dir, store) = temp_repo();
        let content = b"hello world";
        let oid = store.write_blob(content).unwrap();
        let blob = store.repo().find_blob(oid).unwrap();
        assert_eq!(blob.content(), content);
    }

    // -- write_blob_from_file --

    #[test]
    fn write_blob_from_file_regular() {
        let (_dir, store) = temp_repo();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"file content").unwrap();

        let (oid, mode) = store.write_blob_from_file(tmp.path()).unwrap();
        assert_eq!(mode, FileMode::Blob);
        let blob = store.repo().find_blob(oid).unwrap();
        assert_eq!(blob.content(), b"file content");
    }

    #[cfg(unix)]
    #[test]
    fn write_blob_from_file_executable() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, store) = temp_repo();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"#!/bin/sh").unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let (_oid, mode) = store.write_blob_from_file(tmp.path()).unwrap();
        assert_eq!(mode, FileMode::BlobExecutable);
    }

    // -- write_empty_tree --

    #[test]
    fn write_empty_tree() {
        let (_dir, store) = temp_repo();
        let oid = store.write_empty_tree().unwrap();
        let tree = store.repo().find_tree(oid).unwrap();
        assert_eq!(tree.len(), 0);
    }

    // -- write_tree + read_tree roundtrip --

    #[test]
    fn write_and_read_tree_flat() {
        let (_dir, store) = temp_repo();
        let oid1 = store.write_blob(b"content a").unwrap();
        let oid2 = store.write_blob(b"content b").unwrap();

        let mut entries = TreeEntries::new();
        entries.set("a.txt", oid1, FileMode::Blob);
        entries.set("b.txt", oid2, FileMode::Blob);

        let tree_oid = store.write_tree(&entries).unwrap();
        let read_back = store.read_tree(tree_oid).unwrap();

        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back.get("a.txt").unwrap().oid, oid1);
        assert_eq!(read_back.get("b.txt").unwrap().oid, oid2);
    }

    #[test]
    fn write_and_read_tree_nested() {
        let (_dir, store) = temp_repo();
        let oid1 = store.write_blob(b"main").unwrap();
        let oid2 = store.write_blob(b"lib").unwrap();
        let oid3 = store.write_blob(b"readme").unwrap();

        let mut entries = TreeEntries::new();
        entries.set("src/main.rs", oid1, FileMode::Blob);
        entries.set("src/lib/mod.rs", oid2, FileMode::Blob);
        entries.set("README.md", oid3, FileMode::Blob);

        let tree_oid = store.write_tree(&entries).unwrap();
        let read_back = store.read_tree(tree_oid).unwrap();

        assert_eq!(read_back.len(), 3);
        assert_eq!(read_back.get("src/main.rs").unwrap().oid, oid1);
        assert_eq!(read_back.get("src/lib/mod.rs").unwrap().oid, oid2);
        assert_eq!(read_back.get("README.md").unwrap().oid, oid3);
    }

    // -- write_commit --

    #[test]
    fn write_commit_orphan() {
        let (_dir, store) = temp_repo();
        let tree_oid = store.write_empty_tree().unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let commit_oid = store
            .write_commit(tree_oid, &[], "initial commit", &sig)
            .unwrap();
        let commit = store.repo().find_commit(commit_oid).unwrap();
        assert_eq!(commit.parent_count(), 0);
        assert_eq!(commit.message(), Some("initial commit"));
    }

    #[test]
    fn write_commit_with_parent() {
        let (_dir, store) = temp_repo();
        let tree_oid = store.write_empty_tree().unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();

        let parent_oid = store
            .write_commit(tree_oid, &[], "first commit", &sig)
            .unwrap();
        let child_oid = store
            .write_commit(tree_oid, &[parent_oid], "second commit", &sig)
            .unwrap();

        let child = store.repo().find_commit(child_oid).unwrap();
        assert_eq!(child.parent_count(), 1);
        assert_eq!(child.parent_id(0).unwrap(), parent_oid);
    }

    // -- ref operations --

    #[test]
    fn update_and_resolve_ref() {
        let (_dir, store) = temp_repo();
        let tree_oid = store.write_empty_tree().unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let commit_oid = store.write_commit(tree_oid, &[], "initial", &sig).unwrap();

        store.update_ref("test-branch", commit_oid).unwrap();
        let resolved = store.resolve_ref("test-branch").unwrap();
        assert_eq!(resolved, Some(commit_oid));
    }

    #[test]
    fn resolve_ref_nonexistent() {
        let (_dir, store) = temp_repo();
        let resolved = store.resolve_ref("nonexistent").unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn delete_ref_existing() {
        let (_dir, store) = temp_repo();
        let tree_oid = store.write_empty_tree().unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let commit_oid = store.write_commit(tree_oid, &[], "initial", &sig).unwrap();

        store.update_ref("to-delete", commit_oid).unwrap();
        store.delete_ref("to-delete").unwrap();
        assert_eq!(store.resolve_ref("to-delete").unwrap(), None);
    }

    #[test]
    fn delete_ref_nonexistent_is_noop() {
        let (_dir, store) = temp_repo();
        store.delete_ref("nonexistent").unwrap();
    }

    // -- read_blob_at --

    #[test]
    fn read_blob_at_returns_content() {
        let (_dir, store) = temp_repo();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let bs = crate::branchstore::BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();
        bs.write_entry("hello.txt", b"world", "add hello").unwrap();

        let log = bs.log(1).unwrap();
        let commit_oid = log[0].oid;

        let content = store.read_blob_at(commit_oid, "hello.txt").unwrap();
        assert_eq!(content.unwrap(), b"world");
    }

    #[test]
    fn read_blob_at_returns_none_for_missing_path() {
        let (_dir, store) = temp_repo();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let bs = crate::branchstore::BranchStore::new(&store, "test/data", &sig);
        bs.ensure_branch().unwrap();
        bs.write_entry("hello.txt", b"world", "add hello").unwrap();

        let log = bs.log(1).unwrap();
        let commit_oid = log[0].oid;

        let content = store.read_blob_at(commit_oid, "nonexistent.txt").unwrap();
        assert!(content.is_none());
    }
}
