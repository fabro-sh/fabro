//! What Petri admitted for a run.
//!
//! Every run executes on Petri. A run carries the graph Petri lowered and
//! admitted at create time: [`PetriAdmission`] names the root graph and its
//! pre-lowered children by blob and digest. The run executes and resumes
//! from that graph, never from a fresh lowering, so admission-time
//! decisions such as the pinned model routes hold for the run's whole life.

use serde::{Deserialize, Serialize};

use crate::BlobHash;

/// One lowered graph in the blob store: its bytes by hash, and Petri's own
/// content digest of it, which is how a nested-workflow step names its child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PetriGraphRef {
    pub blob:   BlobHash,
    pub digest: String,
}

/// What Petri admitted for a run at create time: the lowered root graph and
/// the pre-lowered child graphs, every one persisted before the run exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PetriAdmission {
    pub graph:    PetriGraphRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PetriGraphRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_admission_without_children_omits_them() {
        let admission = PetriAdmission {
            graph:    PetriGraphRef {
                blob:   BlobHash::new(b"graph"),
                digest: "abc".to_string(),
            },
            children: Vec::new(),
        };
        let value = serde_json::to_value(&admission).expect("admission serializes");
        assert!(value.get("children").is_none());
        let decoded: PetriAdmission = serde_json::from_value(value).expect("admission decodes");
        assert_eq!(decoded, admission);
    }
}
