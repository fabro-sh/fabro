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
            .iter_stages()
            .map(|(stage_id, _)| stage_id.clone())
            .collect();

        for stage_id in stage_ids {
            let Some(node) = projection.stage(&stage_id).cloned() else {
                continue;
            };
            projection.set_stage(stage_id, crate::StageState {
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
