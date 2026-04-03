use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

/// Try to acquire an exclusive (write) lock on `file` without blocking.
///
/// Returns `Ok(true)` if the lock was acquired, `Ok(false)` if another
/// process/fd already holds the lock, and `Err` for unexpected errors.
pub fn try_flock_exclusive(file: &File) -> io::Result<bool> {
    // SAFETY: flock() on a valid fd is safe; LOCK_NB makes it non-blocking.
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Ok(true)
    } else {
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(err),
        }
    }
}

/// Release any lock held on `file`.
pub fn flock_unlock(file: &File) -> io::Result<()> {
    // SAFETY: flock() with LOCK_UN on a valid fd is safe.
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn acquire_exclusive_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file = File::create(&path).unwrap();

        let acquired = try_flock_exclusive(&file).unwrap();
        assert!(acquired);
    }

    #[test]
    fn unlock_then_reacquire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file = File::create(&path).unwrap();

        assert!(try_flock_exclusive(&file).unwrap());
        flock_unlock(&file).unwrap();
        assert!(try_flock_exclusive(&file).unwrap());
    }

    #[test]
    fn second_fd_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file1 = File::create(&path).unwrap();
        let file2 = File::open(&path).unwrap();

        assert!(try_flock_exclusive(&file1).unwrap());
        assert!(!try_flock_exclusive(&file2).unwrap());
    }
}
