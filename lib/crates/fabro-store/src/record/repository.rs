//! Thin typed storage wrapper for simple records that live directly in SlateDB.
//!
//! When adding a new persisted record type:
//! 1. Define the data struct.
//! 2. Implement [`Record`] for it with a stable `PREFIX`, `Id`, and `Codec`.
//! 3. Wrap `Repository<RecordType>` in a small domain store that exposes the
//!    operations callers should use.
//!
//! Example:
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use chrono::{DateTime, Utc};
//! use serde::{Deserialize, Serialize};
//!
//! use crate::record::{JsonCodec, Record, Repository};
//! use crate::Result;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
//! struct Session {
//!     id: String,
//!     user_id: String,
//!     expires_at: DateTime<Utc>,
//! }
//!
//! impl Record for Session {
//!     type Id = String;
//!     type Codec = JsonCodec;
//!     const PREFIX: &'static str = "auth/session";
//!
//!     fn id(&self) -> Self::Id {
//!         self.id.clone()
//!     }
//! }
//!
//! struct SessionStore {
//!     repo: Repository<Session>,
//! }
//!
//! impl SessionStore {
//!     fn new(db: Arc<slatedb::Db>) -> Self {
//!         Self {
//!             repo: Repository::new(db),
//!         }
//!     }
//!
//!     async fn insert(&self, session: Session) -> Result<()> {
//!         self.repo.put(&session).await
//!     }
//!
//!     async fn get(&self, id: &str) -> Result<Option<Session>> {
//!         self.repo.get(&id.to_string()).await
//!     }
//!
//!     async fn gc_expired(&self, now: DateTime<Utc>) -> Result<u64> {
//!         self.repo.gc(|session| session.expires_at <= now).await
//!     }
//! }
//! ```
//!
//! Keep `Repository<R>` internal. Domain-specific invariants such as consume
//! locks, token rotation, or marker-only behavior belong in the named store
//! that wraps it, not in this generic layer.

use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream::{self};
use futures::{Stream, StreamExt};
use slatedb::{Db, KeyValue, WriteBatch};

use super::{Codec, Record, RecordId};
use crate::{Error, Result, keys};

/// Generic typed key/value operations shared by the simple record-backed
/// stores.
///
/// This type is intentionally `pub(crate)`: callers should interact through a
/// named store such as `AuthCodeStore` or `RefreshTokenStore`, which can add
/// domain-specific behavior on top of the generic storage primitives here.
pub(crate) struct Repository<R: Record> {
    db:      Arc<Db>,
    _record: PhantomData<R>,
}

impl<R: Record> Repository<R> {
    pub(crate) fn new(db: Arc<Db>) -> Self {
        validate_prefix::<R>();
        Self {
            db,
            _record: PhantomData,
        }
    }

    pub(crate) async fn get(&self, id: &R::Id) -> Result<Option<R>> {
        self.db
            .get(key_for_id::<R>(id)?)
            .await?
            .map(|bytes| R::Codec::decode(&bytes))
            .transpose()
    }

    pub(crate) async fn put(&self, record: &R) -> Result<()> {
        let id = record.id();
        self.put_at(&id, record).await
    }

    pub(crate) async fn put_at(&self, id: &R::Id, record: &R) -> Result<()> {
        self.db
            .put(key_for_id::<R>(id)?, R::Codec::encode(record)?)
            .await?;
        Ok(())
    }

    pub(crate) async fn delete(&self, id: &R::Id) -> Result<()> {
        self.db.delete(key_for_id::<R>(id)?).await?;
        Ok(())
    }

    pub(crate) async fn exists(&self, id: &R::Id) -> Result<bool> {
        Ok(self.db.get(key_for_id::<R>(id)?).await?.is_some())
    }

    #[allow(
        dead_code,
        reason = "Part of the shared Repository surface; current consumers do not need value scans yet"
    )]
    pub(crate) fn scan_stream(&self) -> RepositoryStream<'_, (R::Id, R)> {
        self.scan_prefix_stream(&[])
    }

    #[allow(
        dead_code,
        reason = "Part of the shared Repository surface; current consumers do not need value scans by sub-prefix yet"
    )]
    pub(crate) fn scan_prefix_stream<'a>(
        &'a self,
        extra_segments: &'a [&'a str],
    ) -> RepositoryStream<'a, (R::Id, R)> {
        match prefix_key::<R>(extra_segments) {
            Ok(prefix) => Box::pin(scan_entries(Arc::clone(&self.db), &prefix).map(|result| {
                result
                    .map_err(Into::into)
                    .and_then(|entry| decode_entry::<R>(&entry))
            })),
            Err(err) => Box::pin(stream::once(async move { Err(err) })),
        }
    }

    pub(crate) fn scan_ids_stream(&self) -> RepositoryStream<'_, R::Id> {
        match prefix_key::<R>(&[]) {
            Ok(prefix) => Box::pin(scan_entries(Arc::clone(&self.db), &prefix).map(|result| {
                result
                    .map_err(Into::into)
                    .and_then(|entry| parse_entry_id::<R>(&entry))
            })),
            Err(err) => Box::pin(stream::once(async move { Err(err) })),
        }
    }

    pub(crate) async fn gc<F>(&self, predicate: F) -> Result<u64>
    where
        F: Fn(&R) -> bool + Send + Sync,
    {
        let mut iter = self.db.scan_prefix(prefix_key::<R>(&[])?).await?;
        let mut batch = WriteBatch::new();
        let mut deletes = 0_u64;

        while let Some(entry) = iter.next().await? {
            let value = R::Codec::decode(&entry.value)?;
            if predicate(&value) {
                batch.delete(entry.key);
                deletes += 1;
            }
        }

        if deletes > 0 {
            self.db.write(batch).await?;
        }

        Ok(deletes)
    }
}

pub(crate) type RepositoryStream<'a, T> = Pin<Box<dyn Stream<Item = Result<T>> + Send + 'a>>;

pub(super) fn key_for_id<R: Record>(id: &R::Id) -> Result<keys::SlateKey> {
    let id_segments = id.key_segments();
    key_from_segments(
        R::PREFIX
            .split('/')
            .chain(id_segments.iter().map(String::as_str)),
    )
}

pub(super) fn prefix_key<R: Record>(extra_segments: &[&str]) -> Result<keys::SlateKey> {
    prefix_from_segments(R::PREFIX.split('/').chain(extra_segments.iter().copied()))
}

fn decode_entry<R: Record>(entry: &KeyValue) -> Result<(R::Id, R)> {
    let id = parse_entry_id::<R>(entry)?;
    let value = R::Codec::decode(&entry.value)?;
    Ok((id, value))
}

fn parse_entry_id<R: Record>(entry: &KeyValue) -> Result<R::Id> {
    let raw_key = String::from_utf8(entry.key.to_vec())
        .map_err(|err| Error::Other(format!("stored key is not valid UTF-8: {err}")))?;
    let segments: Vec<&str> = keys::SlateKey::segments(&raw_key).collect();
    let prefix_len = R::PREFIX.split('/').count();
    if segments.len() < prefix_len {
        return Err(Error::KeyParse(format!(
            "key {raw_key:?} had {} segments, expected at least {} for prefix {}",
            segments.len(),
            prefix_len,
            R::PREFIX
        )));
    }
    if !segments[..prefix_len]
        .iter()
        .copied()
        .eq(R::PREFIX.split('/'))
    {
        return Err(Error::KeyParse(format!(
            "key {raw_key:?} did not match expected prefix {}",
            R::PREFIX
        )));
    }
    R::Id::from_key_segments(&segments[prefix_len..])
}

fn scan_entries(
    db: Arc<Db>,
    prefix: &keys::SlateKey,
) -> impl Stream<Item = std::result::Result<KeyValue, slatedb::Error>> + Send {
    enum ScanState {
        Opening { db: Arc<Db>, prefix: Vec<u8> },
        Iterating(Box<slatedb::DbIterator>),
    }

    stream::try_unfold(
        ScanState::Opening {
            db,
            prefix: prefix.as_ref().to_vec(),
        },
        |state| async move {
            let mut iter = match state {
                ScanState::Opening { db, prefix } => db.scan_prefix(prefix).await?,
                ScanState::Iterating(iter) => *iter,
            };

            match iter.next().await? {
                Some(entry) => Ok(Some((entry, ScanState::Iterating(Box::new(iter))))),
                None => Ok(None),
            }
        },
    )
}

fn validate_prefix<R: Record>() {
    debug_assert!(
        !R::PREFIX.is_empty()
            && !R::PREFIX.starts_with('/')
            && !R::PREFIX.ends_with('/')
            && R::PREFIX.split('/').all(|segment| !segment.is_empty()),
        "Record::PREFIX must be a non-empty '/'-separated path with no empty segments: {}",
        R::PREFIX
    );
}

fn key_from_segments<'a>(segments: impl IntoIterator<Item = &'a str>) -> Result<keys::SlateKey> {
    let mut segments = segments.into_iter();
    let first = segments.next().ok_or_else(|| {
        Error::Other("record key assembly requires at least one segment".to_string())
    })?;
    validate_key_segment(first)?;

    let mut key = keys::SlateKey::new(first);
    for segment in segments {
        validate_key_segment(segment)?;
        key = key.with(segment);
    }
    Ok(key)
}

fn prefix_from_segments<'a>(segments: impl IntoIterator<Item = &'a str>) -> Result<keys::SlateKey> {
    Ok(key_from_segments(segments)?.into_prefix())
}

fn validate_key_segment(segment: &str) -> Result<()> {
    if segment.as_bytes().contains(&b'\0') {
        return Err(Error::InvalidKeySegment {
            segment: segment.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::TryStreamExt;
    use object_store::memory::InMemory;
    use serde::{Deserialize, Serialize};

    use super::Repository;
    use crate::record::{JsonCodec, MarkerCodec, RawBytesCodec, Record, RecordId};
    use crate::{Error, Result};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestRecord {
        id:        TestId,
        payload:   String,
        delete_me: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestId {
        bucket: String,
        name:   String,
    }

    impl RecordId for TestId {
        fn key_segments(&self) -> Vec<String> {
            vec![self.bucket.clone(), self.name.clone()]
        }

        fn from_key_segments(segs: &[&str]) -> Result<Self> {
            let [bucket, name] = segs else {
                return Err(Error::KeyParse(format!(
                    "expected 2 segments for TestId, got {}",
                    segs.len()
                )));
            };
            Ok(Self {
                bucket: (*bucket).to_string(),
                name:   (*name).to_string(),
            })
        }
    }

    impl Record for TestRecord {
        type Id = TestId;
        type Codec = JsonCodec;

        const PREFIX: &'static str = "test/repository";

        fn id(&self) -> Self::Id {
            self.id.clone()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct TestMarker;

    impl Record for TestMarker {
        type Id = String;
        type Codec = MarkerCodec;

        const PREFIX: &'static str = "test/marker";

        fn id(&self) -> Self::Id {
            unreachable!("marker records must use put_at")
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RawBlob(bytes::Bytes);

    impl AsRef<[u8]> for RawBlob {
        fn as_ref(&self) -> &[u8] {
            self.0.as_ref()
        }
    }

    impl From<bytes::Bytes> for RawBlob {
        fn from(value: bytes::Bytes) -> Self {
            Self(value)
        }
    }

    impl Record for RawBlob {
        type Id = String;
        type Codec = RawBytesCodec;

        const PREFIX: &'static str = "test/raw";

        fn id(&self) -> Self::Id {
            "blob".to_string()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct InvalidSegmentRecord {
        id: String,
    }

    impl Record for InvalidSegmentRecord {
        type Id = String;
        type Codec = JsonCodec;

        const PREFIX: &'static str = "test/invalid";

        fn id(&self) -> Self::Id {
            self.id.clone()
        }
    }

    async fn db() -> Arc<slatedb::Db> {
        Arc::new(
            slatedb::Db::open("repository-tests", Arc::new(InMemory::new()))
                .await
                .unwrap(),
        )
    }

    fn record(bucket: &str, name: &str, delete_me: bool) -> TestRecord {
        TestRecord {
            id: TestId {
                bucket: bucket.to_string(),
                name:   name.to_string(),
            },
            payload: format!("{bucket}/{name}"),
            delete_me,
        }
    }

    #[tokio::test]
    async fn put_get_delete_and_scan_round_trip() {
        let repo = Repository::<TestRecord>::new(db().await);
        let saved = record("bucket-a", "alpha", false);

        assert!(repo.get(&saved.id()).await.unwrap().is_none());
        repo.put(&saved).await.unwrap();
        assert_eq!(repo.get(&saved.id()).await.unwrap(), Some(saved.clone()));

        let records = [
            saved.clone(),
            record("bucket-a", "beta", false),
            record("bucket-b", "alpha", false),
            record("bucket-b", "beta", false),
            record("bucket-c", "gamma", false),
        ];
        for record in &records[1..] {
            repo.put(record).await.unwrap();
        }

        let scanned = repo.scan_stream().try_collect::<Vec<_>>().await.unwrap();
        assert_eq!(scanned.len(), records.len());
        assert_eq!(scanned[0], (records[0].id(), records[0].clone()));

        let bucket_a = repo
            .scan_prefix_stream(&["bucket-a"])
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(bucket_a, vec![
            (records[0].id(), records[0].clone()),
            (records[1].id(), records[1].clone()),
        ]);

        repo.delete(&saved.id()).await.unwrap();
        assert!(repo.get(&saved.id()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn gc_deletes_matching_records() {
        let repo = Repository::<TestRecord>::new(db().await);
        for record in [
            record("bucket-a", "keep", false),
            record("bucket-a", "delete", true),
            record("bucket-b", "keep", false),
            record("bucket-b", "delete", true),
        ] {
            repo.put(&record).await.unwrap();
        }

        assert_eq!(repo.gc(|record| record.delete_me).await.unwrap(), 2);

        let remaining = repo.scan_stream().try_collect::<Vec<_>>().await.unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|(_, record)| !record.delete_me));
    }

    #[tokio::test]
    async fn marker_records_use_put_at_exists_and_scan_ids() {
        let repo = Repository::<TestMarker>::new(db().await);
        let marker = TestMarker;

        repo.put_at(&"marker-a".to_string(), &marker).await.unwrap();
        repo.put_at(&"marker-b".to_string(), &marker).await.unwrap();

        assert!(repo.exists(&"marker-a".to_string()).await.unwrap());

        let ids = repo
            .scan_ids_stream()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(ids, vec!["marker-a".to_string(), "marker-b".to_string()]);

        repo.delete(&"marker-a".to_string()).await.unwrap();
        assert!(!repo.exists(&"marker-a".to_string()).await.unwrap());
    }

    #[tokio::test]
    async fn raw_bytes_codec_round_trips_bytes() {
        let repo = Repository::<RawBlob>::new(db().await);
        let blob = RawBlob(bytes::Bytes::from_static(b"hello"));

        repo.put(&blob).await.unwrap();

        assert_eq!(repo.get(&"blob".to_string()).await.unwrap(), Some(blob));
    }

    #[tokio::test]
    async fn malformed_bytes_propagate_decode_errors() {
        let db = db().await;
        let repo = Repository::<TestRecord>::new(Arc::clone(&db));
        db.put(
            super::key_for_id::<TestRecord>(&TestId {
                bucket: "bucket-a".to_string(),
                name:   "broken".to_string(),
            })
            .unwrap(),
            b"not-json",
        )
        .await
        .unwrap();

        let error = repo
            .get(&TestId {
                bucket: "bucket-a".to_string(),
                name:   "broken".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Serde(_)));
    }

    #[tokio::test]
    async fn invalid_key_segments_return_runtime_error() {
        let repo = Repository::<InvalidSegmentRecord>::new(db().await);
        let error = repo
            .put(&InvalidSegmentRecord {
                id: "bad\0segment".to_string(),
            })
            .await
            .unwrap_err();

        match error {
            Error::InvalidKeySegment { segment } => assert_eq!(segment, "bad\0segment"),
            other => panic!("expected invalid key segment error, got {other:?}"),
        }
    }
}
