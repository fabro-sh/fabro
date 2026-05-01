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
            let Some(stage) = projection.stage_mut(&stage_id) else {
                continue;
            };
            stage.prompt = None;
            stage.response = None;
            stage.diff = None;
            stage.stdout = None;
            stage.stderr = None;
        }

        projection.serialize(serializer)
    }
}
