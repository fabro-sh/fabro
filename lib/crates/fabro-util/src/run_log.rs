#![expect(
    clippy::disallowed_types,
    reason = "file-backed tracing sink: sync File is intentional; writes happen on a dedicated \
              per-event guard and are not in an async hot path"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync directory creation and OpenOptions for tracing file appender setup; not on \
              Tokio path"
)]

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

/// File-backed tracing writer that buffers each event and appends it as one
/// contiguous write under a shared lock.
#[derive(Clone, Debug)]
pub struct BufferedFileAppender {
    file: Arc<Mutex<File>>,
}

impl BufferedFileAppender {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().append(true).create(true).open(path)?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
        })
    }
}

impl<'a> MakeWriter<'a> for BufferedFileAppender {
    type Writer = BufferedFileGuard;

    fn make_writer(&'a self) -> Self::Writer {
        BufferedFileGuard {
            buf:  Vec::new(),
            file: self.file.clone(),
        }
    }
}

/// Per-event write guard. Buffers tracing's write calls and flushes the whole
/// event when the guard is dropped.
pub struct BufferedFileGuard {
    buf:  Vec<u8>,
    file: Arc<Mutex<File>>,
}

impl Write for BufferedFileGuard {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for BufferedFileGuard {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(&self.buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::thread;

    use super::*;

    #[test]
    fn buffered_file_appender_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("missing")
            .join("runtime")
            .join("server.log");
        let appender = BufferedFileAppender::open(&path).unwrap();

        let mut guard = appender.make_writer();
        guard.write_all(b"hello world").unwrap();
        drop(guard);

        assert!(path.is_file());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello world");
    }

    #[test]
    fn buffered_file_appender_uses_one_contiguous_flush_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        let appender = BufferedFileAppender::open(&path).unwrap();

        let mut guard = appender.make_writer();
        guard.write_all(b"part1").unwrap();
        guard.write_all(b"part2").unwrap();
        drop(guard);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "part1part2");
    }

    #[test]
    fn buffered_file_appender_does_not_tear_tracing_sized_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        let appender = BufferedFileAppender::open(&path).unwrap();
        let thread_count = 8;
        let lines_per_thread = 100;

        let handles = (0..thread_count)
            .map(|thread_idx| {
                let appender = appender.clone();
                thread::spawn(move || {
                    let marker = (b'a' + u8::try_from(thread_idx).unwrap()) as char;
                    for line_idx in 0..lines_per_thread {
                        let prefix = format!("thread-{thread_idx:02}-line-{line_idx:03}:");
                        let payload = marker.to_string().repeat(256 - prefix.len() - 1);
                        let mut guard = appender.make_writer();
                        guard
                            .write_all(format!("{prefix}{payload}\n").as_bytes())
                            .unwrap();
                        drop(guard);
                    }
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap();
        }

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), thread_count * lines_per_thread);

        for line in lines {
            assert_eq!(line.len(), 255, "line should not be truncated: {line:?}");
            let (prefix, payload) = line.split_once(':').unwrap();
            let thread_idx = prefix
                .strip_prefix("thread-")
                .and_then(|rest| rest.split_once("-line-"))
                .and_then(|(thread_idx, _)| thread_idx.parse::<u8>().ok())
                .unwrap();
            let expected_marker = (b'a' + thread_idx) as char;
            assert!(
                payload.chars().all(|ch| ch == expected_marker),
                "line payload was interleaved: {line:?}"
            );
        }
    }
}
