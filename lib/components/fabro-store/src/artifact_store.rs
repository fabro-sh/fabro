use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use fabro_types::{BlobHash, RunId};
use futures::StreamExt;
use futures::stream::BoxStream;
use object_store::ObjectStore;
use object_store::buffered::BufWriter;
use object_store::path::Path as ObjectPath;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use tokio::io::AsyncWriteExt;

use crate::{Error, Result, StageId};

const ARTIFACT_SEGMENT_ENCODE_SET: &AsciiSet =
    &NON_ALPHANUMERIC.remove(b'.').remove(b'_').remove(b'-');
const STREAM_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArtifactKey {
    pub stage_id:      StageId,
    pub retry:         u32,
    pub relative_path: String,
}

impl ArtifactKey {
    #[must_use]
    pub fn new(stage_id: StageId, retry: u32, relative_path: impl Into<String>) -> Self {
        Self {
            stage_id,
            retry,
            relative_path: relative_path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeArtifact {
    pub node:     StageId,
    pub retry:    u32,
    pub filename: String,
    pub size:     u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StageArtifactEntry {
    pub retry:    u32,
    pub filename: String,
    pub size:     u64,
}

#[derive(Clone)]
pub struct ArtifactStore {
    object_store: Arc<dyn ObjectStore>,
    prefix:       ObjectPath,
}

impl std::fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl ArtifactStore {
    #[must_use]
    pub fn new(object_store: Arc<dyn ObjectStore>, prefix: impl AsRef<str>) -> Self {
        Self {
            object_store,
            prefix: ObjectPath::from(prefix.as_ref()),
        }
    }

    pub async fn put(&self, run_id: &RunId, key: &ArtifactKey, data: &[u8]) -> Result<()> {
        let path = self.artifact_path(run_id, key)?;
        self.object_store
            .put(&path, Bytes::copy_from_slice(data).into())
            .await?;
        Ok(())
    }

    /// Publish a complete captured file under its content digest within the
    /// run. Repeating a put of the same content is safe; metadata is recorded
    /// separately only after this operation succeeds.
    pub async fn put_capture(&self, run_id: &RunId, data: &[u8]) -> Result<BlobHash> {
        let hash = BlobHash::new(data);
        let path = self.capture_prefix(run_id)?.child(hash.to_string());
        self.object_store
            .put(&path, Bytes::copy_from_slice(data).into())
            .await?;
        Ok(hash)
    }

    /// Read run-owned content named by a capture record, without consulting
    /// historical stage keys or the SQLite blob table.
    pub async fn get_capture(&self, run_id: &RunId, hash: &BlobHash) -> Result<Option<Bytes>> {
        let path = self.capture_prefix(run_id)?.child(hash.to_string());
        match self.object_store.get(&path).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn capture_prefix(&self, run_id: &RunId) -> Result<ObjectPath> {
        Ok(self.run_prefix(run_id)?.child("captures").child("sha256"))
    }

    pub fn writer(&self, run_id: &RunId, key: &ArtifactKey) -> Result<BufWriter> {
        let path = self.artifact_path(run_id, key)?;
        Ok(BufWriter::with_capacity(
            Arc::clone(&self.object_store),
            path,
            STREAM_BUFFER_BYTES,
        ))
    }

    pub async fn put_stream<S>(
        &self,
        run_id: &RunId,
        key: &ArtifactKey,
        mut stream: S,
    ) -> Result<()>
    where
        S: futures::Stream<Item = Result<Bytes>> + Unpin,
    {
        let mut writer = self.writer(run_id, key)?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            writer
                .write_all(&chunk)
                .await
                .map_err(|err| Error::Other(format!("artifact write failed: {err}")))?;
        }
        writer
            .shutdown()
            .await
            .map_err(|err| Error::Other(format!("artifact finalize failed: {err}")))?;
        Ok(())
    }

    pub async fn get(&self, run_id: &RunId, key: &ArtifactKey) -> Result<Option<Bytes>> {
        let path = self.artifact_path(run_id, key)?;
        match self.object_store.get(&path).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn get_stream(
        &self,
        run_id: &RunId,
        key: &ArtifactKey,
    ) -> Result<Option<BoxStream<'static, Result<Bytes>>>> {
        let path = self.artifact_path(run_id, key)?;
        match self.object_store.get(&path).await {
            Ok(result) => Ok(Some(
                result
                    .into_stream()
                    .map(|chunk| chunk.map_err(Error::from))
                    .boxed(),
            )),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Check a path against the same rules `put` enforces, without writing.
    ///
    /// Readers that hand artifact paths back to a client — the ZIP download,
    /// for one — use this to re-check paths that were stored before a rule
    /// existed.
    pub fn validate_relative_path(relative_path: &str) -> Result<()> {
        validate_filename_segments(relative_path).map(drop)
    }

    pub async fn list_for_run(&self, run_id: &RunId) -> Result<Vec<NodeArtifact>> {
        let prefix = self.run_prefix(run_id)?;
        let captures = self.capture_prefix(run_id)?;
        let mut stream = self.object_store.list(Some(&prefix));
        let mut artifacts = Vec::new();
        while let Some(meta) = stream.next().await.transpose()? {
            // Capture metadata lives in platform records. Content objects
            // must not be decoded as historical stage/retry/path entries.
            if meta.location.prefix_match(&captures).is_some() {
                continue;
            }
            artifacts.push(decode_artifact_location(
                &prefix,
                &meta.location,
                meta.size,
            )?);
        }
        artifacts.sort();
        Ok(artifacts)
    }

    pub async fn list_for_node(
        &self,
        run_id: &RunId,
        node: &StageId,
    ) -> Result<Vec<StageArtifactEntry>> {
        let prefix = self.node_prefix(run_id, node)?;
        let mut stream = self.object_store.list(Some(&prefix));
        let mut entries = Vec::new();
        while let Some(meta) = stream.next().await.transpose()? {
            entries.push(decode_stage_artifact_entry(
                &prefix,
                &meta.location,
                meta.size,
            )?);
        }
        entries.sort();
        Ok(entries)
    }

    pub async fn delete_for_run(&self, run_id: &RunId) -> Result<()> {
        let prefix = self.run_prefix(run_id)?;
        let mut stream = self.object_store.list(Some(&prefix));
        let mut locations = Vec::new();
        while let Some(meta) = stream.next().await.transpose()? {
            locations.push(meta.location);
        }
        for location in locations {
            self.object_store.delete(&location).await?;
        }
        Ok(())
    }

    pub async fn write_metadata(&self, fabro_version: &str) -> Result<()> {
        let path = parse_object_path(&self.prefixed_raw("store-metadata.json"))?;
        let body = serde_json::to_vec(&serde_json::json!({
            "created_at": Utc::now().to_rfc3339(),
            "fabro_version": fabro_version,
        }))
        .map_err(|err| Error::Other(format!("artifact metadata serialization failed: {err}")))?;
        self.object_store
            .put(&path, Bytes::from(body).into())
            .await?;
        Ok(())
    }

    fn run_prefix(&self, run_id: &RunId) -> Result<ObjectPath> {
        parse_object_path(&self.prefixed_raw(&run_id.to_string()))
    }

    fn node_prefix(&self, run_id: &RunId, node: &StageId) -> Result<ObjectPath> {
        let encoded_node = encode_path_segment(node.node_id());
        parse_object_path(
            &self.prefixed_raw(&format!("{run_id}/{encoded_node}@{:04}", node.visit())),
        )
    }

    fn retry_prefix(&self, run_id: &RunId, node: &StageId, retry: u32) -> Result<ObjectPath> {
        let mut raw = self.node_prefix(run_id, node)?.to_string();
        raw.push('/');
        raw.push_str(&retry_storage_segment(retry));
        parse_object_path(&raw)
    }

    fn artifact_path(&self, run_id: &RunId, key: &ArtifactKey) -> Result<ObjectPath> {
        let mut raw = self
            .retry_prefix(run_id, &key.stage_id, key.retry)?
            .to_string();
        for segment in validate_filename_segments(&key.relative_path)? {
            raw.push('/');
            raw.push_str(&encode_path_segment(segment));
        }
        parse_object_path(&raw)
    }

    fn prefixed_raw(&self, suffix: &str) -> String {
        if self.prefix.as_ref().is_empty() {
            suffix.to_string()
        } else {
            format!("{}/{suffix}", self.prefix)
        }
    }
}

/// Artifact filenames end up as paths on someone else's disk — extracted from a
/// ZIP, written by a worker — so they must stay relative and portable. The
/// backslash, NUL, and drive-letter rules are what make the path safe on
/// Windows as well as Unix.
fn validate_filename_segments(filename: &str) -> Result<Vec<&str>> {
    if filename.contains('\\') {
        return Err(Error::Other(
            "artifact filename must not contain backslashes".to_string(),
        ));
    }
    if filename.contains('\0') {
        return Err(Error::Other(
            "artifact filename must not contain NUL bytes".to_string(),
        ));
    }
    let bytes = filename.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':') {
        return Err(Error::Other(
            "artifact filename must not start with a drive letter".to_string(),
        ));
    }
    let segments = filename.split('/').collect::<Vec<_>>();
    if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        return Err(Error::Other(
            "artifact filename must be a non-empty relative path".to_string(),
        ));
    }
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err(Error::Other(
            "artifact filename must not contain '.' or '..' segments".to_string(),
        ));
    }
    Ok(segments)
}

fn encode_path_segment(segment: &str) -> String {
    utf8_percent_encode(segment, ARTIFACT_SEGMENT_ENCODE_SET).to_string()
}

#[must_use]
pub fn stage_storage_segment(node: &StageId) -> String {
    format!(
        "{}@{:04}",
        encode_path_segment(node.node_id()),
        node.visit()
    )
}

const RETRY_SEGMENT_PREFIX: &str = "retry-";

#[must_use]
pub fn retry_storage_segment(retry: u32) -> String {
    format!("{RETRY_SEGMENT_PREFIX}{retry:04}")
}

fn decode_path_segment(kind: &str, value: &str) -> Result<String> {
    percent_decode_str(value)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|err| Error::Other(format!("invalid {kind}: {err}")))
}

fn decode_artifact_location(
    prefix: &ObjectPath,
    location: &ObjectPath,
    size: u64,
) -> Result<NodeArtifact> {
    let mut parts = location.prefix_match(prefix).ok_or_else(|| {
        Error::Other(format!(
            "artifact location {location} does not match expected prefix {prefix}"
        ))
    })?;
    let stage_part = parts.next().ok_or_else(|| {
        Error::Other(format!(
            "artifact location {location} is missing a stage segment"
        ))
    })?;
    let (encoded_node_id, visit) = stage_part.as_ref().rsplit_once('@').ok_or_else(|| {
        Error::Other(format!(
            "artifact location {location} has an invalid stage segment"
        ))
    })?;
    let node_id = decode_path_segment("artifact node id", encoded_node_id)?;
    let visit = visit.parse::<u32>().map_err(|err| {
        Error::Other(format!(
            "artifact location {location} has an invalid visit number: {err}"
        ))
    })?;
    let (retry, filename) = decode_retry_and_filename(location, &mut parts)?;
    let stage_id = StageId::try_new(node_id, visit).map_err(|err| {
        Error::Other(format!(
            "artifact location {location} has an invalid stage id: {err}"
        ))
    })?;
    Ok(NodeArtifact {
        node: stage_id,
        retry,
        filename,
        size,
    })
}

fn decode_stage_artifact_entry(
    prefix: &ObjectPath,
    location: &ObjectPath,
    size: u64,
) -> Result<StageArtifactEntry> {
    let mut parts = location.prefix_match(prefix).ok_or_else(|| {
        Error::Other(format!(
            "artifact location {location} does not match expected prefix {prefix}"
        ))
    })?;
    let (retry, filename) = decode_retry_and_filename(location, &mut parts)?;
    Ok(StageArtifactEntry {
        retry,
        filename,
        size,
    })
}

fn decode_retry_and_filename<'a, I, P>(
    location: &ObjectPath,
    parts: &mut I,
) -> Result<(u32, String)>
where
    I: Iterator<Item = P>,
    P: AsRef<str> + 'a,
{
    let retry_part = parts.next().ok_or_else(|| {
        Error::Other(format!(
            "artifact location {location} is missing a retry segment"
        ))
    })?;
    let retry = decode_retry_segment(location, retry_part.as_ref())?;
    let filename_segments = parts
        .map(|part| decode_path_segment("artifact filename segment", part.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    if filename_segments.is_empty() {
        return Err(Error::Other(format!(
            "artifact location {location} is missing a filename"
        )));
    }
    Ok((retry, filename_segments.join("/")))
}

fn decode_retry_segment(location: &ObjectPath, segment: &str) -> Result<u32> {
    let Some(value) = segment.strip_prefix(RETRY_SEGMENT_PREFIX) else {
        return Err(Error::Other(format!(
            "artifact location {location} has an invalid retry segment"
        )));
    };
    value.parse::<u32>().map_err(|err| {
        Error::Other(format!(
            "artifact location {location} has an invalid retry number: {err}"
        ))
    })
}

fn parse_object_path(raw: &str) -> Result<ObjectPath> {
    ObjectPath::parse(raw)
        .map_err(|err| Error::Other(format!("invalid artifact object path {raw:?}: {err}")))
}

#[cfg(test)]
mod tests {
    use fabro_types::fixtures;
    use futures::stream;
    use object_store::memory::InMemory;

    use super::*;

    fn test_store() -> ArtifactStore {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        ArtifactStore::new(object_store, "artifacts")
    }

    #[tokio::test]
    async fn captures_are_run_owned_hidden_from_legacy_listing_and_deleted_with_the_run() {
        let objects: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = ArtifactStore::new(objects.clone(), "artifacts");
        let run = RunId::new();
        let other = RunId::new();
        let bytes = b"binary\0capture";
        let hash = fabro_types::BlobHash::new(bytes);
        let legacy = ArtifactKey::new(StageId::new("captures", 1), 1, "sha256/report.bin");
        store.write_metadata("test").await.unwrap();
        store.put(&run, &legacy, b"legacy").await.unwrap();
        for id in [&run, &run, &other] {
            assert_eq!(store.put_capture(id, bytes).await.unwrap(), hash);
        }
        let location = ObjectPath::from(format!("artifacts/{run}/captures/sha256/{hash}"));
        assert_eq!(
            objects.get(&location).await.unwrap().bytes().await.unwrap(),
            bytes.as_slice()
        );
        assert_eq!(
            store.get_capture(&run, &hash).await.unwrap().unwrap(),
            bytes.as_slice()
        );
        assert_eq!(store.list_for_run(&run).await.unwrap().len(), 1);
        assert_eq!(
            store
                .list_for_node(&run, &legacy.stage_id)
                .await
                .unwrap()
                .len(),
            1
        );
        store.delete_for_run(&run).await.unwrap();
        assert!(store.get_capture(&run, &hash).await.unwrap().is_none());
        assert!(store.get(&run, &legacy).await.unwrap().is_none());
        assert_eq!(
            store.get_capture(&other, &hash).await.unwrap().unwrap(),
            bytes.as_slice()
        );
        assert!(
            objects
                .head(&ObjectPath::from("artifacts/store-metadata.json"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn capture_namespace_does_not_hide_malformed_legacy_objects() {
        let objects: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = ArtifactStore::new(objects.clone(), "artifacts");
        let run = RunId::new();
        let location = ObjectPath::from(format!("artifacts/{run}/captures/elsewhere/bad"));
        objects
            .put(&location, Bytes::from_static(b"bad").into())
            .await
            .unwrap();
        assert!(store.list_for_run(&run).await.is_err());
    }

    #[tokio::test]
    async fn write_metadata_persists_store_marker() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = ArtifactStore::new(object_store.clone(), "artifacts");

        store.write_metadata("test-version").await.unwrap();

        let bytes = object_store
            .get(&ObjectPath::from("artifacts/store-metadata.json"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["fabro_version"], "test-version");
        assert!(value["created_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn round_trips_unicode_nodes_and_nested_filenames() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build/naive @ alpha/π", 12);
        let filename = "logs/unicode/naive file ☃.txt";
        let key = ArtifactKey::new(node.clone(), 3, filename);

        store.put(&run_id, &key, b"hello").await.unwrap();

        assert_eq!(
            store.get(&run_id, &key).await.unwrap(),
            Some(Bytes::from_static(b"hello"))
        );
        assert_eq!(store.list_for_node(&run_id, &node).await.unwrap(), vec![
            StageArtifactEntry {
                retry:    3,
                filename: filename.to_string(),
                size:     5,
            }
        ]);
        assert_eq!(store.list_for_run(&run_id).await.unwrap(), vec![
            NodeArtifact {
                node,
                retry: 3,
                filename: filename.to_string(),
                size: 5,
            }
        ]);
    }

    #[tokio::test]
    async fn put_stream_round_trips_chunked_writes() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build", 2);
        let filename = "logs/output.txt";
        let key = ArtifactKey::new(node, 1, filename);

        store
            .put_stream(
                &run_id,
                &key,
                stream::iter(vec![
                    Ok(Bytes::from_static(b"hello ")),
                    Ok(Bytes::from_static(b"world")),
                ]),
            )
            .await
            .unwrap();

        assert_eq!(
            store.get(&run_id, &key).await.unwrap(),
            Some(Bytes::from_static(b"hello world"))
        );
    }

    #[tokio::test]
    async fn get_stream_round_trips_chunked_reads() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let key = ArtifactKey::new(StageId::new("build", 2), 1, "logs/output.txt");
        store.put(&run_id, &key, b"hello world").await.unwrap();

        let mut stream = store.get_stream(&run_id, &key).await.unwrap().unwrap();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk.unwrap());
        }

        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn rejects_invalid_relative_filenames() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build", 1);

        for filename in [
            "",
            "/escape.txt",
            "../escape.txt",
            "logs//output.txt",
            "logs/./output.txt",
            r"logs\output.txt",
            "C:/escape.txt",
            "c:escape.txt",
            "bad\0name.txt",
        ] {
            let key = ArtifactKey::new(node.clone(), 1, filename);
            let err = store.put(&run_id, &key, b"boom").await.unwrap_err();
            assert!(
                err.to_string().contains("artifact filename"),
                "accepted unsafe artifact filename: {filename:?}"
            );
            assert!(ArtifactStore::validate_relative_path(filename).is_err());
        }

        assert!(ArtifactStore::validate_relative_path("nested/result.txt").is_ok());
    }

    #[tokio::test]
    async fn delete_for_run_only_removes_selected_run() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let other_run_id = fixtures::RUN_2;
        let node = StageId::new("build", 1);

        store
            .put(&run_id, &ArtifactKey::new(node.clone(), 1, "a.txt"), b"a")
            .await
            .unwrap();
        store
            .put(
                &run_id,
                &ArtifactKey::new(node.clone(), 1, "nested/b.txt"),
                b"b",
            )
            .await
            .unwrap();
        store
            .put(
                &other_run_id,
                &ArtifactKey::new(node.clone(), 1, "keep.txt"),
                b"keep",
            )
            .await
            .unwrap();

        store.delete_for_run(&run_id).await.unwrap();

        assert!(store.list_for_run(&run_id).await.unwrap().is_empty());
        assert_eq!(
            store.list_for_node(&other_run_id, &node).await.unwrap(),
            vec![StageArtifactEntry {
                retry:    1,
                filename: "keep.txt".to_string(),
                size:     4,
            }]
        );
    }

    #[tokio::test]
    async fn preserves_same_filename_across_retries() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build", 1);
        let first = ArtifactKey::new(node.clone(), 1, "logs/output.txt");
        let second = ArtifactKey::new(node.clone(), 2, "logs/output.txt");

        store.put(&run_id, &first, b"first").await.unwrap();
        store.put(&run_id, &second, b"second").await.unwrap();

        assert_eq!(
            store.get(&run_id, &first).await.unwrap(),
            Some(Bytes::from_static(b"first"))
        );
        assert_eq!(
            store.get(&run_id, &second).await.unwrap(),
            Some(Bytes::from_static(b"second"))
        );
        assert_eq!(store.list_for_node(&run_id, &node).await.unwrap(), vec![
            StageArtifactEntry {
                retry:    1,
                filename: "logs/output.txt".to_string(),
                size:     5,
            },
            StageArtifactEntry {
                retry:    2,
                filename: "logs/output.txt".to_string(),
                size:     6,
            },
        ]);
        assert_eq!(store.list_for_run(&run_id).await.unwrap(), vec![
            NodeArtifact {
                node:     node.clone(),
                retry:    1,
                filename: "logs/output.txt".to_string(),
                size:     5,
            },
            NodeArtifact {
                node,
                retry: 2,
                filename: "logs/output.txt".to_string(),
                size: 6,
            },
        ]);
    }

    #[tokio::test]
    async fn rejects_legacy_artifact_paths_without_retry_segment() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = ArtifactStore::new(object_store.clone(), "artifacts");
        let run_id = fixtures::RUN_1;
        object_store
            .put(
                &ObjectPath::from(format!("artifacts/{run_id}/build@0001/output.txt")),
                Bytes::from_static(b"legacy").into(),
            )
            .await
            .unwrap();

        let err = store.list_for_run(&run_id).await.unwrap_err();

        assert!(err.to_string().contains("invalid retry segment"));
        assert!(err.to_string().contains("build@0001/output.txt"));
    }
}
