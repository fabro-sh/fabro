use serde::{Serialize, Serializer};

use crate::RunProjection;

pub struct SerializableProjection<'a>(pub &'a RunProjection);

impl Serialize for SerializableProjection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut projection = self.0.clone();
        let stage_ids: Vec<_> = projection
            .iter_nodes()
            .map(|(stage_id, _)| stage_id.clone())
            .collect();

        for stage_id in stage_ids {
            let Some(node) = projection.node(&stage_id).cloned() else {
                continue;
            };
            projection.set_node(stage_id, crate::NodeState {
                prompt: None,
                response: None,
                diff: None,
                stdout: None,
                stderr: None,
                ..node
            });
        }

        projection.serialize(serializer)
    }
}
