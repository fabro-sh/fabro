//! The admitted graphs in Fabro's blob store.
//!
//! What `Runtime::check` admitted is what the run executes and resumes from,
//! so the root graph and every pre-lowered child are serialized into the
//! blob store at create time and named on the run spec as a
//! [`PetriAdmission`]: the blob by hash, and Petri's own content digest,
//! which is the key the coordinator registers the graph under and the name a
//! nested-workflow step invokes its child by. Loading verifies the digest,
//! so a blob that does not decode to the graph it claims is refused.
//!
//! The server loads through its [`BlobStore`]; a run's worker loads through
//! its client's blob read with [`load_with`], since the run's blobs are the
//! blob store the server answers `GET /runs/{id}/blobs/{hash}` from.

use std::future::Future;

use fabro_store::BlobStore;
use fabro_types::{BlobHash, PetriAdmission, PetriGraphRef};
use petri_runtime::frontend::graph_digest;
use petri_runtime::ir::Graph;

use crate::check::Admitted;

/// The graphs a run starts from: the admitted root and its pre-lowered
/// children, loaded and verified.
pub struct AdmittedGraphs {
    pub graph:    Graph,
    pub children: Vec<Graph>,
}

/// Why an admission could not be stored or loaded.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    #[error("the blob store failed")]
    Store(#[source] fabro_store::Error),
    #[error("blob `{blob}` could not be read")]
    Read {
        blob:   String,
        #[source]
        source: anyhow::Error,
    },
    #[error("graph `{digest}` is not in the blob store")]
    Missing { digest: String },
    #[error("graph `{digest}` does not encode as JSON")]
    Encode {
        digest: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("blob `{digest}` does not decode as a graph")]
    Decode {
        digest: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("blob `{blob}` decodes to graph `{found}`, not `{digest}`")]
    DigestMismatch {
        blob:   String,
        digest: String,
        found:  String,
    },
}

/// Serialize the admitted graphs into `blobs` and name them.
pub async fn persist(
    blobs: &BlobStore,
    admitted: &Admitted,
) -> Result<PetriAdmission, AdmissionError> {
    let graph = persist_graph(blobs, &admitted.graph).await?;
    let mut children = Vec::with_capacity(admitted.children.len());
    for child in &admitted.children {
        children.push(persist_graph(blobs, child).await?);
    }
    Ok(PetriAdmission { graph, children })
}

/// The root graph and its children, read back from `blobs` and checked
/// against their digests.
pub async fn load(
    blobs: &BlobStore,
    admission: &PetriAdmission,
) -> Result<AdmittedGraphs, AdmissionError> {
    load_with(
        |blob| async move { blobs.read(&blob).await.map_err(anyhow::Error::from) },
        admission,
    )
    .await
}

/// [`load`] over any blob read: `read` answers a hash with the blob's
/// bytes, or `None` when the store lacks it.
pub async fn load_with<F, Fut>(
    read: F,
    admission: &PetriAdmission,
) -> Result<AdmittedGraphs, AdmissionError>
where
    F: Fn(BlobHash) -> Fut,
    Fut: Future<Output = anyhow::Result<Option<bytes::Bytes>>>,
{
    let graph = load_graph(&read, &admission.graph).await?;
    let mut children = Vec::with_capacity(admission.children.len());
    for child in &admission.children {
        children.push(load_graph(&read, child).await?);
    }
    Ok(AdmittedGraphs { graph, children })
}

async fn persist_graph(blobs: &BlobStore, graph: &Graph) -> Result<PetriGraphRef, AdmissionError> {
    let digest = graph_digest(graph);
    let bytes = serde_json::to_vec(graph).map_err(|source| AdmissionError::Encode {
        digest: digest.clone(),
        source,
    })?;
    let blob = blobs.write(&bytes).await.map_err(AdmissionError::Store)?;
    Ok(PetriGraphRef { blob, digest })
}

async fn load_graph<F, Fut>(read: &F, graph: &PetriGraphRef) -> Result<Graph, AdmissionError>
where
    F: Fn(BlobHash) -> Fut,
    Fut: Future<Output = anyhow::Result<Option<bytes::Bytes>>>,
{
    let bytes = read(graph.blob)
        .await
        .map_err(|source| AdmissionError::Read {
            blob: graph.blob.to_string(),
            source,
        })?
        .ok_or_else(|| AdmissionError::Missing {
            digest: graph.digest.clone(),
        })?;
    let decoded: Graph =
        serde_json::from_slice(&bytes).map_err(|source| AdmissionError::Decode {
            digest: graph.digest.clone(),
            source,
        })?;
    let found = graph_digest(&decoded);
    if found != graph.digest {
        return Err(AdmissionError::DigestMismatch {
            blob: graph.blob.to_string(),
            digest: graph.digest.clone(),
            found,
        });
    }
    Ok(decoded)
}
