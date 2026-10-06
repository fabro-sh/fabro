//! A lock one process holds on a file for its whole life.
//!
//! The kernel ends the lock when its process exits, however it exits, so a
//! free lock proves the process that held it is gone, and the kernel names
//! the process that holds a lock to any other process that asks. The lock
//! is a POSIX record lock (`fcntl`), not an `flock`, because only a record
//! lock names its holder. Two rules follow from record-lock semantics: the
//! holder must not open the file a second time, since closing any
//! descriptor of the file ends the process's record locks on it; and a
//! process never sees its own lock as held.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::fs::OpenOptions;
use tokio::time;

use crate::signal;

/// How often [`stop_lock_holder`] checks whether the lock is free.
const POLL: Duration = Duration::from_millis(20);

#[allow(
    clippy::cast_possible_truncation,
    clippy::unnecessary_cast,
    reason = "the lock constants are c_int on Linux and c_short on macOS, where flock's fields \
              are c_short on both; every value is small"
)]
mod consts {
    pub(super) const WRITE_LOCK: libc::c_short = libc::F_WRLCK as libc::c_short;
    pub(super) const UNLOCKED: libc::c_short = libc::F_UNLCK as libc::c_short;
    pub(super) const FROM_START: libc::c_short = libc::SEEK_SET as libc::c_short;
}

/// The lock this process holds on a file, until the lock is dropped or the
/// process exits.
#[derive(Debug)]
pub struct ProcessLock {
    _file: File,
}

impl ProcessLock {
    /// Take the lock on the file at `path`, created when missing, for this
    /// process, without waiting. `Ok(None)` when another process holds it.
    pub async fn try_hold(path: &Path) -> io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .await?
            .into_std()
            .await;
        let mut lock = whole_file(consts::WRITE_LOCK);
        // SAFETY: fcntl(F_SETLK) on a valid descriptor with a valid flock
        // struct; F_SETLK does not wait.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &raw mut lock) } == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EACCES | libc::EAGAIN) => Ok(None),
            _ => Err(error),
        }
    }
}

/// What [`stop_lock_holder`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockHolder {
    /// No other process held the lock.
    None,
    /// This process held the lock. It was killed, with its process group,
    /// and the lock is free: it is gone.
    Stopped { pid: u32 },
}

/// Stop the process that holds the lock on the file at `path`, if another
/// process does, and wait up to `patience` for the lock to be free. The
/// holder and its process group get `SIGKILL`: a holder that could handle
/// a signal could also keep running. A missing file has no holder.
///
/// `Err` when the lock is still held after `patience`.
pub async fn stop_lock_holder(path: &Path, patience: Duration) -> io::Result<LockHolder> {
    let file = match OpenOptions::new().read(true).write(true).open(path).await {
        Ok(file) => file.into_std().await,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LockHolder::None),
        Err(error) => return Err(error),
    };
    let Some(pid) = holder(&file)? else {
        return Ok(LockHolder::None);
    };
    signal::sigkill_process_group(pid);
    signal::sigkill(pid);
    let deadline = Instant::now() + patience;
    loop {
        match holder(&file)? {
            None => return Ok(LockHolder::Stopped { pid }),
            Some(_) if Instant::now() >= deadline => {
                return Err(io::Error::other(format!(
                    "process {pid} still holds {} after SIGKILL",
                    path.display()
                )));
            }
            Some(_) => time::sleep(POLL).await,
        }
    }
}

/// The process that holds the lock on `file`, if another process does.
fn holder(file: &File) -> io::Result<Option<u32>> {
    let mut lock = whole_file(consts::WRITE_LOCK);
    // SAFETY: fcntl(F_GETLK) on a valid descriptor with a valid flock struct
    // only reads the lock table.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut lock) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if lock.l_type == consts::UNLOCKED {
        return Ok(None);
    }
    u32::try_from(lock.l_pid).map(Some).map_err(|_| {
        io::Error::other(format!(
            "the lock on the file is held, by no process id ({})",
            lock.l_pid
        ))
    })
}

/// A lock request covering the whole file, however long it grows.
fn whole_file(kind: libc::c_short) -> libc::flock {
    // SAFETY: flock is a plain C struct, for which all zeros is valid.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = kind;
    lock.l_whence = consts::FROM_START;
    lock.l_start = 0;
    lock.l_len = 0;
    lock
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    clippy::print_stdout,
    reason = "the tests start this test binary as the process that holds a lock, and it says so \
              on its stdout"
)]
mod tests {
    use std::process::Stdio;

    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    use super::*;

    /// Set for the test binary started as a lock's holder: the lock's path.
    const HOLD: &str = "FABRO_PROC_TEST_HOLD_LOCK";

    /// Not a test: the process the other tests start to hold a lock. It
    /// holds the lock at `$FABRO_PROC_TEST_HOLD_LOCK`, says so, and waits
    /// to be killed. Without the variable it does nothing.
    #[tokio::test]
    async fn hold_the_lock_until_killed() {
        let Some(path) = std::env::var_os(HOLD) else {
            return;
        };
        let lock = ProcessLock::try_hold(Path::new(&path))
            .await
            .expect("the lock file opens")
            .expect("the lock is free");
        println!("held");
        time::sleep(Duration::from_mins(1)).await;
        drop(lock);
    }

    /// This test binary, holding the lock at `path` in a process group of
    /// its own, once it says it holds it.
    async fn holder_process(path: &Path) -> Child {
        let mut command = Command::new(std::env::current_exe().expect("the test binary"));
        command
            .args([
                "--exact",
                "process_lock::tests::hold_the_lock_until_killed",
                "--nocapture",
            ])
            .env(HOLD, path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::pre_exec_setpgid(command.as_std_mut());
        let mut child = command.spawn().expect("the holder starts");
        let stdout = child.stdout.take().expect("the holder's stdout");
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.expect("the holder's stdout reads") {
            if line.trim() == "held" {
                return child;
            }
        }
        panic!("the holder exited before it held the lock");
    }

    #[tokio::test]
    async fn a_missing_or_free_lock_has_no_holder() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("worker.lock");
        assert_eq!(
            stop_lock_holder(&path, Duration::from_secs(1))
                .await
                .expect("the check runs"),
            LockHolder::None
        );
        drop(
            ProcessLock::try_hold(&path)
                .await
                .expect("the file opens")
                .expect("the lock is free"),
        );
        assert_eq!(
            stop_lock_holder(&path, Duration::from_secs(1))
                .await
                .expect("the check runs"),
            LockHolder::None
        );
    }

    #[tokio::test]
    async fn a_lock_another_process_holds_cannot_be_taken() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("worker.lock");
        let _holder = holder_process(&path).await;
        assert!(
            ProcessLock::try_hold(&path)
                .await
                .expect("the file opens")
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_holder_is_killed_and_the_lock_is_free_once_it_is_gone() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("worker.lock");
        let mut holder = holder_process(&path).await;
        let pid = holder.id().expect("the holder runs");

        let found = stop_lock_holder(&path, Duration::from_secs(10))
            .await
            .expect("the holder is stopped");

        assert_eq!(found, LockHolder::Stopped { pid });
        let status = holder.wait().await.expect("the holder is reaped");
        assert!(!status.success(), "the holder was killed: {status}");
        assert!(
            ProcessLock::try_hold(&path)
                .await
                .expect("the file opens")
                .is_some(),
            "the lock is free"
        );
    }
}
