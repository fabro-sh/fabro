use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;

static RUN_LOG: OnceLock<RunLogWriter> = OnceLock::new();

/// A switchable `MakeWriter` that can be activated/deactivated at runtime.
///
/// When inactive, writes are silently discarded with no allocation.
/// When active, each event is buffered per-guard and flushed atomically on drop.
#[derive(Clone, Debug)]
pub struct RunLogWriter {
    active: Arc<AtomicBool>,
    file: Arc<Mutex<Option<BufWriter<std::fs::File>>>>,
}

impl RunLogWriter {
    fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            file: Arc::new(Mutex::new(None)),
        }
    }
}

impl<'a> MakeWriter<'a> for RunLogWriter {
    type Writer = RunLogGuard;

    fn make_writer(&'a self) -> Self::Writer {
        if self.active.load(Ordering::Relaxed) {
            RunLogGuard::Active {
                buf: Vec::new(),
                file: self.file.clone(),
            }
        } else {
            RunLogGuard::Inactive
        }
    }
}

/// Per-event write guard. Buffers writes and flushes atomically on drop.
pub enum RunLogGuard {
    Inactive,
    Active {
        buf: Vec<u8>,
        file: Arc<Mutex<Option<BufWriter<std::fs::File>>>>,
    },
}

impl Write for RunLogGuard {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Self::Inactive => Ok(data.len()),
            Self::Active { buf, .. } => buf.write(data),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for RunLogGuard {
    fn drop(&mut self) {
        if let Self::Active { buf, file } = self {
            if buf.is_empty() {
                return;
            }
            if let Ok(mut guard) = file.lock() {
                if let Some(writer) = guard.as_mut() {
                    let _ = writer.write_all(buf);
                    let _ = writer.flush();
                }
            }
        }
    }
}

/// Initialize the global run log writer. Returns a clone for use as a tracing layer writer.
///
/// Must be called exactly once (typically from logging init). Panics on second call.
pub fn init() -> RunLogWriter {
    let writer = RunLogWriter::new();
    let clone = writer.clone();
    RUN_LOG
        .set(writer)
        .expect("run_log::init() called more than once");
    clone
}

/// Activate per-run logging, directing tracing output to `path`.
pub fn activate(path: &Path) -> io::Result<()> {
    let writer = RUN_LOG
        .get()
        .expect("run_log::activate() called before init()");
    let file = std::fs::File::create(path)?;
    let mut guard = writer.file.lock().expect("run log lock poisoned");
    *guard = Some(BufWriter::new(file));
    writer.active.store(true, Ordering::Release);
    Ok(())
}

/// Deactivate per-run logging. Flushes and closes the current log file.
pub fn deactivate() {
    let Some(writer) = RUN_LOG.get() else {
        return;
    };
    writer.active.store(false, Ordering::Release);
    let mut guard = writer.file.lock().expect("run log lock poisoned");
    if let Some(mut w) = guard.take() {
        let _ = w.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Helper: create a standalone `RunLogWriter` (not the global singleton) for isolated tests.
    fn test_writer() -> RunLogWriter {
        RunLogWriter::new()
    }

    #[test]
    fn inactive_writer_discards_writes() {
        let w = test_writer();
        let mut guard = w.make_writer();
        guard.write_all(b"should be discarded").unwrap();
        drop(guard);
        // No file, no panic — writes silently dropped
    }

    #[test]
    fn activate_writes_to_file() {
        let w = test_writer();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");

        // Activate
        let file = std::fs::File::create(&path).unwrap();
        *w.file.lock().unwrap() = Some(BufWriter::new(file));
        w.active.store(true, Ordering::Release);

        // Write via guard
        let mut guard = w.make_writer();
        guard.write_all(b"hello world").unwrap();
        drop(guard);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello world");
    }

    #[test]
    fn deactivate_stops_writing() {
        let w = test_writer();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");

        // Activate
        let file = std::fs::File::create(&path).unwrap();
        *w.file.lock().unwrap() = Some(BufWriter::new(file));
        w.active.store(true, Ordering::Release);

        // Write while active
        let mut guard = w.make_writer();
        guard.write_all(b"before").unwrap();
        drop(guard);

        // Deactivate
        w.active.store(false, Ordering::Release);
        if let Some(mut bw) = w.file.lock().unwrap().take() {
            let _ = bw.flush();
        }

        // Write while inactive — should be discarded
        let mut guard = w.make_writer();
        guard.write_all(b"after").unwrap();
        drop(guard);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "before");
    }

    #[test]
    fn atomic_writes_no_interleaving() {
        let w = test_writer();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");

        // Activate
        let file = std::fs::File::create(&path).unwrap();
        *w.file.lock().unwrap() = Some(BufWriter::new(file));
        w.active.store(true, Ordering::Release);

        // Multiple write() calls on the same guard should produce one contiguous block
        let mut guard = w.make_writer();
        guard.write_all(b"part1").unwrap();
        guard.write_all(b"part2").unwrap();
        drop(guard);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "part1part2");
    }
}
