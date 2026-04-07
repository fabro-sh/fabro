use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use object_store::{ObjectStore, path::Path as ObjectPath};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use tokio::io::AsyncWriteExt;

use crate::{Result, StageId, StoreError};
use fabro_types::RunId;

const ARTIFACT_SEGMENT_ENCODE_SET: &AsciiSet =
    &NON_ALPHANUMERIC.remove(b'.').remove(b'_').remove(b'-');
const STREAM_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeArtifact {
    pub node: StageId,
    pub filename: String,
    pub size: u64,
}

#[derive(Clone)]
pub struct ArtifactStore {
    object_store: Arc<dyn ObjectStore>,
    prefix: ObjectPath,
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

    pub async fn put(
        &self,
        run_id: &RunId,
        node: &StageId,
        filename: &str,
        data: &[u8],
    ) -> Result<()> {
        let path = self.artifact_path(run_id, node, filename)?;
        self.object_store
            .put(&path, Bytes::copy_from_slice(data).into())
            .await?;
        Ok(())
    }

    pub fn writer(
        &self,
        run_id: &RunId,
        node: &StageId,
        filename: &str,
    ) -> Result<object_store::buffered::BufWriter> {
        let path = self.artifact_path(run_id, node, filename)?;
        Ok(object_store::buffered::BufWriter::with_capacity(
            Arc::clone(&self.object_store),
            path,
            STREAM_BUFFER_BYTES,
        ))
    }

    pub async fn put_stream<S>(
        &self,
        run_id: &RunId,
        node: &StageId,
        filename: &str,
        mut stream: S,
    ) -> Result<()>
    where
        S: futures::Stream<Item = Result<Bytes>> + Unpin,
    {
        let mut writer = self.writer(run_id, node, filename)?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            writer
                .write_all(&chunk)
                .await
                .map_err(|err| StoreError::Other(format!("artifact write failed: {err}")))?;
        }
        writer
            .shutdown()
            .await
            .map_err(|err| StoreError::Other(format!("artifact finalize failed: {err}")))?;
        Ok(())
    }

    pub async fn get(
        &self,
        run_id: &RunId,
        node: &StageId,
        filename: &str,
    ) -> Result<Option<Bytes>> {
        let path = self.artifact_path(run_id, node, filename)?;
        match self.object_store.get(&path).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn list_for_run(&self, run_id: &RunId) -> Result<Vec<NodeArtifact>> {
        let prefix = self.run_prefix(run_id)?;
        let mut stream = self.object_store.list(Some(&prefix));
        let mut artifacts = Vec::new();
        while let Some(meta) = stream.next().await.transpose()? {
            artifacts.push(decode_artifact_location(
                &prefix,
                &meta.location,
                meta.size,
            )?);
        }
        artifacts.sort();
        Ok(artifacts)
    }

    pub async fn list_for_node(&self, run_id: &RunId, node: &StageId) -> Result<Vec<String>> {
        let prefix = self.node_prefix(run_id, node)?;
        let mut stream = self.object_store.list(Some(&prefix));
        let mut filenames = Vec::new();
        while let Some(meta) = stream.next().await.transpose()? {
            filenames.push(decode_filename(&prefix, &meta.location)?);
        }
        filenames.sort();
        Ok(filenames)
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

    fn run_prefix(&self, run_id: &RunId) -> Result<ObjectPath> {
        parse_object_path(&self.prefixed_raw(&run_id.to_string()))
    }

    fn node_prefix(&self, run_id: &RunId, node: &StageId) -> Result<ObjectPath> {
        let encoded_node = encode_path_segment(node.node_id());
        parse_object_path(
            &self.prefixed_raw(&format!("{run_id}/{encoded_node}@{:04}", node.visit())),
        )
    }

    fn artifact_path(&self, run_id: &RunId, node: &StageId, filename: &str) -> Result<ObjectPath> {
        let mut raw = self.node_prefix(run_id, node)?.to_string();
        for segment in validate_filename_segments(filename)? {
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

fn validate_filename_segments(filename: &str) -> Result<Vec<&str>> {
    if filename.contains('\\') {
        return Err(StoreError::Other(
            "artifact filename must not contain backslashes".to_string(),
        ));
    }
    let segments = filename.split('/').collect::<Vec<_>>();
    if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
        return Err(StoreError::Other(
            "artifact filename must be a non-empty relative path".to_string(),
        ));
    }
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err(StoreError::Other(
            "artifact filename must not contain '.' or '..' segments".to_string(),
        ));
    }
    Ok(segments)
}

fn encode_path_segment(segment: &str) -> String {
    utf8_percent_encode(segment, ARTIFACT_SEGMENT_ENCODE_SET).to_string()
}

fn decode_path_segment(kind: &str, value: &str) -> Result<String> {
    percent_decode_str(value)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|err| StoreError::Other(format!("invalid {kind}: {err}")))
}

fn decode_artifact_location(
    prefix: &ObjectPath,
    location: &ObjectPath,
    size: u64,
) -> Result<NodeArtifact> {
    let mut parts = location.prefix_match(prefix).ok_or_else(|| {
        StoreError::Other(format!(
            "artifact location {location} does not match expected prefix {prefix}"
        ))
    })?;
    let stage_part = parts.next().ok_or_else(|| {
        StoreError::Other(format!(
            "artifact location {location} is missing a stage segment"
        ))
    })?;
    let (encoded_node_id, visit) = stage_part.as_ref().rsplit_once('@').ok_or_else(|| {
        StoreError::Other(format!(
            "artifact location {location} has an invalid stage segment"
        ))
    })?;
    let node_id = decode_path_segment("artifact node id", encoded_node_id)?;
    let visit = visit.parse::<u32>().map_err(|err| {
        StoreError::Other(format!(
            "artifact location {location} has an invalid visit number: {err}"
        ))
    })?;
    let filename_segments = parts
        .map(|part| decode_path_segment("artifact filename segment", part.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    if filename_segments.is_empty() {
        return Err(StoreError::Other(format!(
            "artifact location {location} is missing a filename"
        )));
    }
    Ok(NodeArtifact {
        node: StageId::new(node_id, visit),
        filename: filename_segments.join("/"),
        size,
    })
}

fn decode_filename(prefix: &ObjectPath, location: &ObjectPath) -> Result<String> {
    let mut parts = location.prefix_match(prefix).ok_or_else(|| {
        StoreError::Other(format!(
            "artifact location {location} does not match expected prefix {prefix}"
        ))
    })?;
    let filename_segments = parts
        .by_ref()
        .map(|part| decode_path_segment("artifact filename segment", part.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    if filename_segments.is_empty() {
        return Err(StoreError::Other(format!(
            "artifact location {location} is missing a filename"
        )));
    }
    Ok(filename_segments.join("/"))
}

fn parse_object_path(raw: &str) -> Result<ObjectPath> {
    ObjectPath::parse(raw)
        .map_err(|err| StoreError::Other(format!("invalid artifact object path {raw:?}: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use object_store::memory::InMemory;

    use fabro_types::fixtures;

    fn test_store() -> ArtifactStore {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        ArtifactStore::new(object_store, "artifacts")
    }

    #[tokio::test]
    async fn round_trips_unicode_nodes_and_nested_filenames() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build/naive @ alpha/π", 12);
        let filename = "logs/unicode/naive file ☃.txt";

        store.put(&run_id, &node, filename, b"hello").await.unwrap();

        assert_eq!(
            store.get(&run_id, &node, filename).await.unwrap(),
            Some(Bytes::from_static(b"hello"))
        );
        assert_eq!(
            store.list_for_node(&run_id, &node).await.unwrap(),
            vec![filename.to_string()]
        );
        assert_eq!(
            store.list_for_run(&run_id).await.unwrap(),
            vec![NodeArtifact {
                node,
                filename: filename.to_string(),
                size: 5,
            }]
        );
    }

    #[tokio::test]
    async fn put_stream_round_trips_chunked_writes() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build", 2);
        let filename = "logs/output.txt";

        store
            .put_stream(
                &run_id,
                &node,
                filename,
                futures::stream::iter(vec![
                    Ok(Bytes::from_static(b"hello ")),
                    Ok(Bytes::from_static(b"world")),
                ]),
            )
            .await
            .unwrap();

        assert_eq!(
            store.get(&run_id, &node, filename).await.unwrap(),
            Some(Bytes::from_static(b"hello world"))
        );
    }

    #[tokio::test]
    async fn rejects_invalid_relative_filenames() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let node = StageId::new("build", 1);

        for filename in [
            "",
            "../escape.txt",
            "logs//output.txt",
            "logs/./output.txt",
            r"logs\output.txt",
        ] {
            let err = store
                .put(&run_id, &node, filename, b"boom")
                .await
                .unwrap_err();
            assert!(err.to_string().contains("artifact filename"));
        }
    }

    #[tokio::test]
    async fn delete_for_run_only_removes_selected_run() {
        let store = test_store();
        let run_id = fixtures::RUN_1;
        let other_run_id = fixtures::RUN_2;
        let node = StageId::new("build", 1);

        store.put(&run_id, &node, "a.txt", b"a").await.unwrap();
        store
            .put(&run_id, &node, "nested/b.txt", b"b")
            .await
            .unwrap();
        store
            .put(&other_run_id, &node, "keep.txt", b"keep")
            .await
            .unwrap();

        store.delete_for_run(&run_id).await.unwrap();

        assert!(store.list_for_run(&run_id).await.unwrap().is_empty());
        assert_eq!(
            store.list_for_node(&other_run_id, &node).await.unwrap(),
            vec!["keep.txt".to_string()]
        );
    }
}
