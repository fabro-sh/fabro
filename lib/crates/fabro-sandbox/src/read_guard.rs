use std::collections::HashSet;
use std::path::{Component, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::{debug, warn};

use crate::Sandbox;

/// Decorator that prevents writing to files the agent hasn't read first.
///
/// Tracks which file paths the agent has seen (via `mark_agent_read`, called by
/// tool executors after agent-visible reads) and returns an error when
/// `write_file` or `delete_file` targets an existing file that hasn't been
/// read. Writing to new (non-existent) files is always allowed.
pub struct ReadBeforeWriteSandbox {
    inner:    Arc<dyn Sandbox>,
    read_set: Mutex<HashSet<String>>,
}

impl ReadBeforeWriteSandbox {
    pub fn new(inner: Arc<dyn Sandbox>) -> Self {
        Self {
            inner,
            read_set: Mutex::new(HashSet::new()),
        }
    }

    fn normalize_path(&self, path: &str) -> String {
        let full = if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            PathBuf::from(self.inner.working_directory()).join(path)
        };

        let mut parts: Vec<String> = Vec::new();
        for component in full.components() {
            match component {
                Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
                Component::ParentDir => {
                    parts.pop();
                }
                Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            }
        }

        format!("/{}", parts.join("/"))
    }

    fn mark_read(&self, path: &str) {
        let normalized = self.normalize_path(path);
        self.read_set
            .lock()
            .expect("read_set lock poisoned")
            .insert(normalized);
    }

    fn has_read(&self, path: &str) -> bool {
        let normalized = self.normalize_path(path);
        self.read_set
            .lock()
            .expect("read_set lock poisoned")
            .contains(&normalized)
    }

    async fn guard_write(&self, path: &str) -> Result<(), String> {
        let normalized = self.normalize_path(path);
        if normalized.starts_with("/tmp/") {
            return Ok(());
        }
        let exists = self.inner.file_exists(path).await?;
        if exists && !self.has_read(path) {
            warn!(path = %path, "Write blocked: file not read by agent");
            Err(format!(
                "Cannot write to '{path}': file exists but has not been read. \
                 Use read_file to read the file before writing to it."
            ))
        } else {
            Ok(())
        }
    }
}

crate::delegate_sandbox! {
    ReadBeforeWriteSandbox => inner {
        async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
            self.guard_write(path).await?;
            self.inner.write_file(path, content).await
        }

        async fn delete_file(&self, path: &str) -> Result<(), String> {
            self.guard_write(path).await?;
            self.inner.delete_file(path).await
        }

        fn mark_agent_read(&self, path: &str) {
            debug!(path = %path, "File marked as agent-read");
            self.mark_read(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::GrepOptions;
    use crate::test_support::MockSandbox;

    fn mock_with_files(files: HashMap<String, String>) -> MockSandbox {
        MockSandbox {
            files,
            working_dir: "/work",
            ..Default::default()
        }
    }

    // Cycle 1: write to existing unread file → error
    #[tokio::test]
    async fn write_to_existing_unread_file_returns_error() {
        let mock = mock_with_files(HashMap::from([("a.ts".into(), "content".into())]));
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        let result = env.write_file("a.ts", "new content").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("a.ts"));
        assert!(err.contains("read"));
    }

    // Cycle 2: write to non-existent file → success
    #[tokio::test]
    async fn write_to_nonexistent_file_succeeds() {
        let mock = mock_with_files(HashMap::new());
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        let result = env.write_file("new.ts", "content").await;

        assert!(result.is_ok());
    }

    // Cycle 3: mark_agent_read then write → success
    #[tokio::test]
    async fn read_then_write_succeeds() {
        let mock = mock_with_files(HashMap::from([("a.ts".into(), "content".into())]));
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        env.mark_agent_read("a.ts");
        let result = env.write_file("a.ts", "new content").await;

        assert!(result.is_ok());
    }

    // Cycle 4: read_file alone does NOT satisfy guard
    #[tokio::test]
    async fn read_file_alone_does_not_satisfy_guard() {
        let mock = mock_with_files(HashMap::from([("a.ts".into(), "content".into())]));
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        env.read_file("a.ts", None, None).await.unwrap();
        let result = env.write_file("a.ts", "new content").await;

        assert!(result.is_err());
    }

    // Cycle 5: grep alone does NOT populate read set
    #[tokio::test]
    async fn grep_does_not_populate_read_set() {
        let mock = MockSandbox {
            files: HashMap::from([("b.ts".into(), "content".into())]),
            grep_results: vec!["b.ts:1:content".into()],
            working_dir: "/work",
            ..Default::default()
        };
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        env.grep("pattern", ".", &GrepOptions::default())
            .await
            .unwrap();
        let result = env.write_file("b.ts", "new").await;

        assert!(result.is_err());
    }

    // Cycle 6: mark_agent_read from grep results then write → success
    #[tokio::test]
    async fn mark_agent_read_from_grep_then_write_succeeds() {
        let mock = MockSandbox {
            files: HashMap::from([("b.ts".into(), "content".into())]),
            grep_results: vec!["b.ts:1:content".into()],
            working_dir: "/work",
            ..Default::default()
        };
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        env.mark_agent_read("b.ts");
        let result = env.write_file("b.ts", "new").await;

        assert!(result.is_ok());
    }

    // Cycle 7: glob does NOT populate read set
    #[tokio::test]
    async fn glob_does_not_populate_read_set() {
        let mock = MockSandbox {
            files: HashMap::from([("c.ts".into(), "content".into())]),
            glob_results: vec!["c.ts".into()],
            working_dir: "/work",
            ..Default::default()
        };
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        env.glob("*.ts", None).await.unwrap();
        let result = env.write_file("c.ts", "new").await;

        assert!(result.is_err());
    }

    // Cycle 8: path normalization — relative vs absolute via mark_agent_read
    #[tokio::test]
    async fn path_normalization_relative_and_absolute() {
        let mock = MockSandbox {
            files: HashMap::from([
                ("a.ts".into(), "content".into()),
                ("/work/a.ts".into(), "content".into()),
            ]),
            working_dir: "/work",
            ..Default::default()
        };
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        env.mark_agent_read("a.ts");
        let result = env.write_file("/work/a.ts", "new content").await;

        assert!(result.is_ok());
    }

    // Cycle 9: delete unread file → error
    #[tokio::test]
    async fn delete_unread_file_returns_error() {
        let mock = mock_with_files(HashMap::from([("d.ts".into(), "content".into())]));
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        let result = env.delete_file("d.ts").await;

        assert!(result.is_err());
    }

    // Cycle 10: error message is actionable
    #[tokio::test]
    async fn error_message_is_actionable() {
        let mock = mock_with_files(HashMap::from([("main.rs".into(), "fn main() {}".into())]));
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        let err = env.write_file("main.rs", "new").await.unwrap_err();

        assert!(err.contains("main.rs"));
        assert!(err.contains("read_file"));
    }

    // Cycle 11: write to /tmp bypasses guard
    #[tokio::test]
    async fn write_to_tmp_bypasses_guard() {
        let mock = MockSandbox {
            files: HashMap::from([("/tmp/fabro-commit-msg".into(), "old".into())]),
            working_dir: "/work",
            ..Default::default()
        };
        let env = ReadBeforeWriteSandbox::new(Arc::new(mock));

        let result = env.write_file("/tmp/fabro-commit-msg", "new").await;

        assert!(result.is_ok());
    }
}
