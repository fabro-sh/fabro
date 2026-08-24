//! Per-path serialization for parallel write tools.
//!
//! `edit_file` mutates a file with a read-modify-write cycle against the
//! sandbox. When one assistant response fans out multiple write calls to the
//! same file, unconstrained parallel execution lets them clobber each other:
//! both calls read the same base content, each writes its own variant, and
//! the last write silently discards the other call's edit (observed in run
//! `01M0NJZGX7BMJAJC3CZJZ0RNT0`: two parallel `edit_file` calls to `main.go`
//! lost a `strconv` import and two call-site updates).
//!
//! The dispatcher creates one [`BatchWriteLocks`] map per parallel tool-call
//! batch and threads it into the executors via
//! [`crate::tool_registry::ToolContext`]. Write tools acquire the per-path
//! mutex for the whole read-modify-write span, so same-file calls serialize
//! while different-file calls stay concurrent. Locking is best-effort by
//! lexical path: keys are normalized without touching the filesystem (the
//! sandbox may be remote) and different spellings of the same file simply fail
//! to share a lock.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// Per-batch map from normalized path to that path's write mutex.
pub type BatchWriteLocks = Arc<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>>;

/// Create the lock map for one parallel tool-call batch.
#[must_use]
pub fn new_batch_write_locks() -> BatchWriteLocks {
    Arc::default()
}

/// Lexically normalize a path for use as a lock key: drop `.` components and
/// resolve `..` without filesystem access.
fn lock_key(path: &str) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Acquire the write lock for `path`, warning when another same-batch write
/// call already holds it (the silent-clobber case this module exists for).
pub async fn lock_write_path(locks: &BatchWriteLocks, path: &str) -> OwnedMutexGuard<()> {
    let mutex = {
        let mut map = locks.lock().expect("write lock map poisoned");
        map.entry(lock_key(path))
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };
    match mutex.clone().try_lock_owned() {
        Ok(guard) => guard,
        Err(_would_block) => {
            tracing::warn!(
                path = path,
                "concurrent write to the same file in one batch; serializing",
            );
            mutex.lock_owned().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_key_normalizes_lexically() {
        assert_eq!(lock_key("/a/./b/c.go"), PathBuf::from("/a/b/c.go"));
        assert_eq!(lock_key("/a/b/../c.go"), PathBuf::from("/a/c.go"));
        assert_eq!(lock_key("relative.go"), PathBuf::from("relative.go"));
    }

    #[tokio::test]
    async fn same_path_serializes_and_different_paths_do_not() {
        let locks = new_batch_write_locks();
        let first = lock_write_path(&locks, "/a/./b/c.go").await;
        assert!(
            locks
                .lock()
                .unwrap()
                .get(&PathBuf::from("/a/b/c.go"))
                .is_some()
        );
        drop(first);

        let _a = lock_write_path(&locks, "/x.go").await;
        let _b = lock_write_path(&locks, "/y.go").await;
    }
}
