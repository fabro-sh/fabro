use serde::{Serialize, Serializer};

use crate::RunProjection;

pub struct SerializableProjection<'a>(pub &'a RunProjection);

impl Serialize for SerializableProjection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut projection = self.0.clone();
        for (_, stage) in projection.iter_stages_mut() {
            stage.prompt = None;
            stage.response = None;
            stage.diff = None;
            stage.stdout = None;
            stage.stderr = None;
        }

        projection.serialize(serializer)
    }
}
