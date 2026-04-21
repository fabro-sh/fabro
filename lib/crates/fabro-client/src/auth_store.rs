#![expect(
    clippy::disallowed_methods,
    reason = "CLI auth storage is local file I/O, not a hot async path."
)]
#![expect(
    clippy::disallowed_types,
    reason = "CLI auth storage requires std::fs::File handles for advisory locking."
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::target::ServerTarget;

const AUTH_FILE_ENV: &str = "FABRO_AUTH_FILE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSubject {
    pub idp_issuer:  String,
    pub idp_subject: String,
    pub login:       String,
    pub name:        String,
    pub email:       String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthEntry {
    pub access_token:             String,
    pub access_token_expires_at:  DateTime<Utc>,
    pub refresh_token:            String,
    pub refresh_token_expires_at: DateTime<Utc>,
    pub subject:                  StoredSubject,
    pub logged_in_at:             DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum AuthStoreError {
    #[allow(
        dead_code,
        reason = "This platform-gated variant is exercised on non-Unix targets."
    )]
    #[error("CLI OAuth login is not supported on this platform in this release.")]
    UnsupportedPlatform,
    #[error("invalid server target `{value}`")]
    InvalidServerTarget { value: String },
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("failed to read auth store at {path}: {source}")]
    Read {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse auth store at {path}: {source}")]
    Corrupt {
        path:   PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to create auth store directory {path}: {source}")]
    CreateDir {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write auth store at {path}: {source}")]
    Write {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize auth store at {path}: {source}")]
    Serialize {
        path:   PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug, Error)]
pub enum LockError {
    #[error(
        "the filesystem backing {path} does not support file locking; move the auth store to a local filesystem or set {AUTH_FILE_ENV} to a local path"
    )]
    FilesystemDoesNotSupportLocking { path: PathBuf },
    #[error("failed to lock auth store at {path}: {source}")]
    Io {
        path:   PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct AuthStore {
    path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthFile {
    #[serde(default)]
    servers: BTreeMap<String, AuthEntry>,
}

impl Default for AuthStore {
    fn default() -> Self {
        let path = std::env::var_os(AUTH_FILE_ENV).map_or_else(
            || fabro_util::Home::from_env().root().join("auth.json"),
            PathBuf::from,
        );
        Self::new(path)
    }
}

impl AuthStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn get(&self, target: &ServerTarget) -> Result<Option<AuthEntry>, AuthStoreError> {
        let key = key_for_target(target);
        self.with_shared_lock(|| {
            let file = self.read_auth_file()?;
            Ok(file.servers.get(&key).cloned())
        })
    }

    pub fn put(&self, target: &ServerTarget, entry: AuthEntry) -> Result<(), AuthStoreError> {
        #[cfg(not(unix))]
        {
            let _ = (target, entry);
            Err(AuthStoreError::UnsupportedPlatform)
        }

        #[cfg(unix)]
        {
            let key = key_for_target(target);
            self.ensure_parent_dir()?;
            self.with_exclusive_lock(|| {
                let mut file = self.read_auth_file()?;
                file.servers.insert(key, entry);
                self.write_auth_file(&file)
            })
        }
    }

    pub fn remove(&self, target: &ServerTarget) -> Result<bool, AuthStoreError> {
        #[cfg(not(unix))]
        {
            let _ = target;
            Err(AuthStoreError::UnsupportedPlatform)
        }

        #[cfg(unix)]
        {
            let key = key_for_target(target);
            self.ensure_parent_dir()?;
            self.with_exclusive_lock(|| {
                let mut file = self.read_auth_file()?;
                let removed = file.servers.remove(&key).is_some();
                if removed {
                    self.write_auth_file(&file)?;
                }
                Ok(removed)
            })
        }
    }

    pub fn list(&self) -> Result<Vec<(ServerTarget, AuthEntry)>, AuthStoreError> {
        self.with_shared_lock(|| {
            let file = self.read_auth_file()?;
            file.servers
                .into_iter()
                .map(|(key, entry)| Ok((parse_stored_target(&key)?, entry)))
                .collect::<Result<Vec<_>, AuthStoreError>>()
        })
    }

    fn read_auth_file(&self) -> Result<AuthFile, AuthStoreError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                serde_json::from_str(&contents).map_err(|source| AuthStoreError::Corrupt {
                    path: self.path.clone(),
                    source,
                })
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(AuthFile::default()),
            Err(source) => Err(AuthStoreError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    #[cfg(unix)]
    fn ensure_parent_dir(&self) -> Result<(), AuthStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| AuthStoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn with_shared_lock<T>(
        &self,
        f: impl FnOnce() -> Result<T, AuthStoreError>,
    ) -> Result<T, AuthStoreError> {
        let lock_file = self.open_lock_file()?;
        match FileExt::try_lock_shared(&lock_file) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                lock_file
                    .lock_shared()
                    .map_err(|source| self.lock_error(source))?;
            }
            Err(source) => return Err(self.lock_error(source)),
        }
        f()
    }

    #[cfg(not(unix))]
    fn with_shared_lock<T>(
        &self,
        f: impl FnOnce() -> Result<T, AuthStoreError>,
    ) -> Result<T, AuthStoreError> {
        f()
    }

    #[cfg(unix)]
    fn with_exclusive_lock<T>(
        &self,
        f: impl FnOnce() -> Result<T, AuthStoreError>,
    ) -> Result<T, AuthStoreError> {
        let lock_file = self.open_lock_file()?;
        match FileExt::try_lock_exclusive(&lock_file) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                lock_file
                    .lock_exclusive()
                    .map_err(|source| self.lock_error(source))?;
            }
            Err(source) => return Err(self.lock_error(source)),
        }
        f()
    }

    #[cfg(unix)]
    fn open_lock_file(&self) -> Result<std::fs::File, AuthStoreError> {
        let path = self.lock_path();
        std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| LockError::Io { path, source }.into())
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    #[cfg(unix)]
    fn lock_error(&self, source: std::io::Error) -> AuthStoreError {
        classify_lock_error(self.lock_path(), source).into()
    }

    #[cfg(unix)]
    fn write_auth_file(&self, file: &AuthFile) -> Result<(), AuthStoreError> {
        let serialized =
            serde_json::to_string_pretty(file).map_err(|source| AuthStoreError::Serialize {
                path: self.path.clone(),
                source,
            })?;

        let temp_path = self.path.with_file_name(format!(
            ".{}.tmp-{:x}",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("auth"),
            rand::rng().random::<u64>()
        ));

        write_private_file(&temp_path, &format!("{serialized}\n"))?;
        fs::rename(&temp_path, &self.path).map_err(|source| AuthStoreError::Write {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }
}

fn key_for_target(target: &ServerTarget) -> String {
    target.to_string()
}

fn parse_stored_target(value: &str) -> Result<ServerTarget, AuthStoreError> {
    if let Some(path) = value.strip_prefix("unix://") {
        return ServerTarget::unix_socket_path(path).map_err(|_| {
            AuthStoreError::InvalidServerTarget {
                value: value.to_string(),
            }
        });
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return ServerTarget::http_url(value).map_err(|_| AuthStoreError::InvalidServerTarget {
            value: value.to_string(),
        });
    }
    Err(AuthStoreError::InvalidServerTarget {
        value: value.to_string(),
    })
}

#[cfg(unix)]
fn write_private_file(path: &Path, contents: &str) -> Result<(), AuthStoreError> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| AuthStoreError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(contents.as_bytes())
        .map_err(|source| AuthStoreError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.sync_all().map_err(|source| AuthStoreError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(unix)]
fn classify_lock_error(path: PathBuf, source: std::io::Error) -> LockError {
    match source.raw_os_error() {
        Some(code) if code == libc::EOPNOTSUPP || code == libc::ENOLCK => {
            LockError::FilesystemDoesNotSupportLocking { path }
        }
        _ => LockError::Io { path, source },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;

    use chrono::Duration;

    #[cfg(unix)]
    use super::{AUTH_FILE_ENV, LockError, classify_lock_error};
    use super::{AuthEntry, AuthStore, StoredSubject, key_for_target};
    use crate::target::ServerTarget;

    fn entry(login: &str) -> AuthEntry {
        let now = chrono::Utc::now();
        AuthEntry {
            access_token:             format!("access-{login}"),
            access_token_expires_at:  now + Duration::minutes(10),
            refresh_token:            format!("refresh-{login}"),
            refresh_token_expires_at: now + Duration::days(30),
            subject:                  StoredSubject {
                idp_issuer:  "https://github.com".to_string(),
                idp_subject: "12345".to_string(),
                login:       login.to_string(),
                name:        format!("Name {login}"),
                email:       format!("{login}@example.com"),
            },
            logged_in_at:             now,
        }
    }

    fn https_target(value: &str) -> ServerTarget {
        ServerTarget::http_url(value).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_https_entry() {
        let temp = tempfile::tempdir().unwrap();
        let store = AuthStore::new(temp.path().join("auth.json"));
        let target = https_target("https://fabro.example.com");

        store.put(&target, entry("octocat")).unwrap();

        let saved = store.get(&target).unwrap().unwrap();
        assert_eq!(saved.subject.login, "octocat");
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_loopback_http_entry() {
        let temp = tempfile::tempdir().unwrap();
        let store = AuthStore::new(temp.path().join("auth.json"));
        let target = https_target("http://127.0.0.1:3000");

        store.put(&target, entry("alice")).unwrap();

        let saved = store.get(&target).unwrap().unwrap();
        assert_eq!(saved.subject.login, "alice");
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_unix_socket_entry() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("fabro.sock");
        std::fs::write(&socket, "").unwrap();
        let store = AuthStore::new(temp.path().join("auth.json"));
        let target = ServerTarget::unix_socket_path(socket).unwrap();

        store.put(&target, entry("unix")).unwrap();

        let saved = store.get(&target).unwrap().unwrap();
        assert_eq!(saved.subject.login, "unix");
    }

    #[test]
    fn https_normalization_collapses_equivalent_urls() {
        let a = key_for_target(&https_target("https://EXAMPLE.COM/"));
        let b = key_for_target(&https_target("https://example.com:443"));
        let c = key_for_target(&https_target("https://example.com"));

        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn distinct_unix_socket_paths_do_not_collide() {
        let a = key_for_target(&ServerTarget::unix_socket_path("/tmp/a.sock").unwrap());
        let b = key_for_target(&ServerTarget::unix_socket_path("/tmp/b.sock").unwrap());

        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_distinct_symlinked_socket_paths() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("fabro.sock");
        let link = temp.path().join("fabro-link.sock");
        std::fs::write(&socket, "").unwrap();
        std::os::unix::fs::symlink(&socket, &link).unwrap();

        let direct = key_for_target(&ServerTarget::unix_socket_path(socket).unwrap());
        let via_link = key_for_target(&ServerTarget::unix_socket_path(link).unwrap());

        assert_ne!(direct, via_link);
    }

    #[test]
    fn missing_file_returns_empty_results() {
        let temp = tempfile::tempdir().unwrap();
        let store = AuthStore::new(temp.path().join("auth.json"));
        let target = https_target("https://fabro.example.com");

        assert!(store.get(&target).unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn corrupt_file_returns_clear_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        std::fs::write(&path, "{not-json").unwrap();
        let store = AuthStore::new(path.clone());
        let target = https_target("https://fabro.example.com");

        let err = store.get(&target).unwrap_err();
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_puts_do_not_corrupt_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(AuthStore::new(temp.path().join("auth.json")));
        let target = https_target("https://fabro.example.com");

        let mut tasks = Vec::new();
        for login in ["alice", "bob"] {
            let store = Arc::clone(&store);
            let target = target.clone();
            tasks.push(thread::spawn(move || {
                store.put(&target, entry(login)).unwrap();
            }));
        }
        for task in tasks {
            task.join().unwrap();
        }

        let saved = store.get(&target).unwrap().unwrap();
        assert!(matches!(saved.subject.login.as_str(), "alice" | "bob"));
    }

    #[cfg(unix)]
    #[test]
    fn writes_files_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let store = AuthStore::new(temp.path().join("auth.json"));
        let target = https_target("https://fabro.example.com");

        store.put(&target, entry("octocat")).unwrap();

        let mode = std::fs::metadata(temp.path().join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(not(unix))]
    #[test]
    fn put_returns_unsupported_platform() {
        let temp = tempfile::tempdir().unwrap();
        let store = AuthStore::new(temp.path().join("auth.json"));
        let target = https_target("https://fabro.example.com");

        let err = store.put(&target, entry("octocat")).unwrap_err();
        assert!(err.to_string().contains("not supported on this platform"));
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_locking_filesystem_returns_actionable_error() {
        let path = PathBuf::from("/tmp/fabro-auth.lock");
        let err = classify_lock_error(
            path.clone(),
            std::io::Error::from_raw_os_error(libc::ENOLCK),
        );

        assert!(matches!(
            err,
            LockError::FilesystemDoesNotSupportLocking { path: ref error_path } if error_path == &path
        ));
        assert!(err.to_string().contains(AUTH_FILE_ENV));
    }
}
