use fabro_types::{RunEvent, RunId};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventPayload(serde_json::Value);

impl EventPayload {
    pub fn new(value: serde_json::Value, expected_run_id: &RunId) -> Result<Self> {
        let payload = Self(value);
        payload.validate(expected_run_id)?;
        Ok(payload)
    }

    pub(crate) fn validate(&self, expected_run_id: &RunId) -> Result<()> {
        let obj = self
            .0
            .as_object()
            .ok_or_else(|| Error::InvalidEvent("event payload must be a JSON object".into()))?;

        for field in ["id", "ts", "run_id", "event"] {
            match obj.get(field) {
                Some(serde_json::Value::String(_)) => {}
                _ => {
                    return Err(Error::InvalidEvent(format!(
                        "missing or non-string required field: {field}"
                    )));
                }
            }
        }

        match obj.get("run_id") {
            Some(serde_json::Value::String(run_id)) if run_id == &expected_run_id.to_string() => {
                Ok(())
            }
            Some(serde_json::Value::String(run_id)) => Err(Error::InvalidEvent(format!(
                "payload run_id {run_id:?} does not match store run_id {expected_run_id:?}"
            ))),
            _ => Err(Error::InvalidEvent(
                "missing or non-string required field: run_id".into(),
            )),
        }
    }

    pub fn into_inner(self) -> serde_json::Value {
        self.0
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl TryFrom<&EventPayload> for RunEvent {
    type Error = Error;

    fn try_from(value: &EventPayload) -> Result<Self> {
        Self::from_ref(value.as_value())
            .map_err(|err| Error::InvalidEvent(format!("invalid stored event: {err}")))
    }
}
