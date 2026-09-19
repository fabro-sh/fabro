//! The run stream: one ordered delivery of a Petri run's public events and
//! Fabro's platform records, as `GET /runs/{id}/events` and the attach
//! stream serve them for a run that executes on Petri.
//!
//! Each item is a Petri `RunEvent` (Petri's event contract, passed through
//! as JSON) or a stored platform record (Fabro's own fact about the run:
//! its lifecycle before and after the engine, a checkpoint with its commit,
//! a pull request), in one Fabro envelope. The envelope carries:
//!
//! - `stream_seq`, the durable per-run delivery sequence the projector assigned
//!   when the item's record was committed. It is the cursor: a client resumes
//!   from the last `stream_seq` it saw. It is dense and strictly increasing
//!   within a run.
//! - `id`, the item's own identity, kept beside the cursor so a client
//!   deduplicates by it: for a Petri event the `EventId` as
//!   `<log>/<seq>/<index>` (`coordinator/3/0`, `execution 1/23/0`), for a
//!   platform record its `seq`. Petri's `EventId` is per log and has no
//!   platform variant, so it is never the cursor.
//! - `kind`, which of the two the item is.
//! - `recorded_at`, when the item's record was appended, in milliseconds since
//!   the Unix epoch; the same field both item shapes carry.
//! - `item`, the Petri `RunEvent` or the stored platform record, unchanged.

use serde::{Deserialize, Serialize};

use crate::RunId;

/// Which of the two item shapes a stream item carries.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum RunStreamItemKind {
    /// A Petri `RunEvent`: `{id, origin, recorded_at, context, subject,
    /// record, derived}` under Petri's event contract.
    Petri,
    /// A stored platform record: `{seq, recorded_at, record: {kind, ...},
    /// position?}`.
    Platform,
}

/// One item of a Petri run's stream, in Fabro's envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStreamItem {
    pub run_id:      RunId,
    /// The delivery sequence: the cursor.
    pub stream_seq:  u64,
    pub kind:        RunStreamItemKind,
    /// The item's own identity, for deduplication.
    pub id:          String,
    /// Milliseconds since the Unix epoch when the item's record was
    /// appended.
    pub recorded_at: u64,
    /// The Petri `RunEvent` or the stored platform record, as JSON.
    pub item:        serde_json::Value,
}

impl RunStreamItem {
    /// The `<subject>.<verb>` name of a Petri event, or the `kind` of a
    /// platform record: what a listing shows and a filter matches on.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self.kind {
            RunStreamItemKind::Petri => petri_event_name(&self.item),
            RunStreamItemKind::Platform => self
                .item
                .get("record")
                .and_then(|record| record.get("kind"))
                .and_then(serde_json::Value::as_str),
        }
    }
}

/// The `<subject>.<verb>` name of a Petri `RunEvent` value: the recorded
/// body's `event` tag, or a view event's tag under `derived`.
#[must_use]
pub fn petri_event_name(event: &serde_json::Value) -> Option<&str> {
    event
        .get("record")
        .and_then(|record| record.get("body"))
        .and_then(|body| body.get("event"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            event
                .get("derived")
                .and_then(|derived| derived.get("event"))
                .and_then(serde_json::Value::as_str)
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RunStreamItem, RunStreamItemKind, petri_event_name};
    use crate::fixtures;

    #[test]
    fn kind_names_are_lowercase_in_both_directions() {
        assert_eq!(RunStreamItemKind::Petri.to_string(), "petri");
        assert_eq!(
            "platform".parse::<RunStreamItemKind>(),
            Ok(RunStreamItemKind::Platform)
        );
        assert_eq!(
            serde_json::to_value(RunStreamItemKind::Platform).expect("kind serializes"),
            json!("platform")
        );
    }

    #[test]
    fn a_petri_item_is_named_by_its_recorded_event_tag() {
        let item = RunStreamItem {
            run_id:      fixtures::RUN_1,
            stream_seq:  4,
            kind:        RunStreamItemKind::Petri,
            id:          "coordinator/3/0".to_string(),
            recorded_at: 1_789_323_217_366,
            item:        json!({
                "id": {"log": "coordinator", "seq": 3, "index": 0},
                "record": {"seq": 3, "body": {"event": "execution.declared"}}
            }),
        };
        assert_eq!(item.name(), Some("execution.declared"));
        assert_eq!(
            petri_event_name(&json!({"origin": "derived", "derived": {"event": "visit.started"}})),
            Some("visit.started")
        );
    }

    #[test]
    fn a_platform_item_is_named_by_its_record_kind() {
        let item = RunStreamItem {
            run_id:      fixtures::RUN_1,
            stream_seq:  5,
            kind:        RunStreamItemKind::Platform,
            id:          "2".to_string(),
            recorded_at: 1_789_323_217_400,
            item:        json!({"seq": 2, "record": {"kind": "run.notice", "level": "info"}}),
        };
        assert_eq!(item.name(), Some("run.notice"));
    }

    #[test]
    fn the_envelope_round_trips_as_json() {
        let value = json!({
            "run_id": fixtures::RUN_1.to_string(),
            "stream_seq": 7,
            "kind": "platform",
            "id": "3",
            "recorded_at": 1_789_323_217_400_u64,
            "item": {"seq": 3, "recorded_at": 1_789_323_217_400_u64, "record": {"kind": "checkpoint", "execution": 1, "firing": 2}}
        });
        let item: RunStreamItem = serde_json::from_value(value.clone()).expect("item decodes");
        assert_eq!(serde_json::to_value(&item).expect("item encodes"), value);
    }
}
