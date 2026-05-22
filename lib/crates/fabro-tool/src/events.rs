use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::EventEnvelope;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common;
use super::common::{FabroToolBackend, ToolError, ToolResult};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunEventsAction {
    List,
    Details,
    Search,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FabroRunEventsParams {
    pub action:             RunEventsAction,
    pub run_id:             String,
    pub event_types:        Option<Vec<String>>,
    pub categories:         Option<Vec<String>>,
    pub direction:          Option<String>,
    pub created_after:      Option<String>,
    pub created_before:     Option<String>,
    pub first:              Option<usize>,
    pub after:              Option<u32>,
    pub event_ids:          Option<Vec<String>>,
    pub offset:             Option<usize>,
    pub limit:              Option<usize>,
    pub max_content_length: Option<usize>,
    pub query:              Option<String>,
}

#[derive(Debug)]
pub struct ValidatedRunEvents {
    pub raw:            FabroRunEventsParams,
    pub descending:     bool,
    pub first:          usize,
    pub created_after:  Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
}

impl TryFrom<FabroRunEventsParams> for ValidatedRunEvents {
    type Error = ToolError;

    fn try_from(params: FabroRunEventsParams) -> Result<Self, Self::Error> {
        if params.run_id.trim().is_empty() {
            return Err(ToolError::message("run_id is required"));
        }
        let first = params.first.or(params.limit).unwrap_or(50);
        if first > 200 {
            return Err(ToolError::message("first must be <= 200"));
        }
        let descending = match params.direction.as_deref() {
            None | Some("asc") => false,
            Some("desc") => true,
            Some(_) => return Err(ToolError::message("direction must be `asc` or `desc`")),
        };
        let created_after = params
            .created_after
            .as_deref()
            .map(|created_after| common::parse_datetime_filter("created_after", created_after))
            .transpose()?;
        let created_before = params
            .created_before
            .as_deref()
            .map(|created_before| common::parse_datetime_filter("created_before", created_before))
            .transpose()?;
        if matches!(params.action, RunEventsAction::Details)
            && params.event_ids.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(ToolError::message(
                "event_ids is required for details action",
            ));
        }
        if matches!(params.action, RunEventsAction::Search)
            && params
                .query
                .as_deref()
                .is_none_or(|query| query.trim().is_empty())
        {
            return Err(ToolError::message("query is required for search action"));
        }
        Ok(Self {
            raw: params,
            descending,
            first,
            created_after,
            created_before,
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunEventsResult {
    pub run_id:      String,
    pub action:      RunEventsAction,
    pub events:      Vec<RunEventResult>,
    pub next_cursor: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunEventResult {
    pub event_id:  String,
    pub sequence:  u32,
    pub event:     Value,
    pub truncated: bool,
}

pub async fn run_events(
    backend: Arc<dyn FabroToolBackend>,
    params: ValidatedRunEvents,
) -> ToolResult<RunEventsResult> {
    let descending = params.descending;
    let first = params.first;
    let created_after = params.created_after;
    let created_before = params.created_before;
    let raw = params.raw;
    let run_id = backend
        .resolve_run(&raw.run_id)
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?
        .id;
    let fetch_after = if descending { None } else { raw.after };
    let mut events = if let Some(limit) = event_fetch_limit(&raw, first) {
        backend
            .list_run_events_until(&run_id, fetch_after, limit)
            .await
    } else {
        backend.list_run_events(&run_id, fetch_after, None).await
    }
    .map_err(|err| ToolError::from_anyhow(&err))?;
    if descending {
        if let Some(after) = raw.after {
            events.retain(|event| event.seq < after);
        }
    }
    filter_events(&mut events, &raw, created_after, created_before);
    if descending {
        events.reverse();
    }
    let offset = raw.offset.unwrap_or(0);
    let page = events
        .into_iter()
        .skip(offset)
        .take(first)
        .collect::<Vec<_>>();
    let max_content_length = raw.max_content_length.unwrap_or(20_000);
    let results = page
        .iter()
        .map(|event| run_event_result(event, max_content_length))
        .collect::<ToolResult<Vec<_>>>()?;
    let next_cursor = page.last().map(|event| {
        if descending {
            event.seq
        } else {
            event.seq.saturating_add(1)
        }
    });

    Ok(RunEventsResult {
        run_id: run_id.to_string(),
        action: raw.action,
        events: results,
        next_cursor,
    })
}

pub fn run_events_text(result: &RunEventsResult) -> String {
    format!("returned {} Fabro event(s)", result.events.len())
}

fn event_fetch_limit(params: &FabroRunEventsParams, first: usize) -> Option<usize> {
    let needs_full_scan = params.event_ids.is_some()
        || params.event_types.is_some()
        || params.categories.is_some()
        || params.created_after.is_some()
        || params.created_before.is_some()
        || params.direction.as_deref() == Some("desc")
        || matches!(
            params.action,
            RunEventsAction::Details | RunEventsAction::Search
        );
    if needs_full_scan {
        return None;
    }

    let requested = first.saturating_add(params.offset.unwrap_or(0));
    Some(requested.max(1))
}

fn filter_events(
    events: &mut Vec<EventEnvelope>,
    params: &FabroRunEventsParams,
    created_after: Option<DateTime<Utc>>,
    created_before: Option<DateTime<Utc>>,
) {
    if let Some(event_ids) = params.event_ids.as_ref() {
        events.retain(|event| event_ids.contains(&event.event.id));
    }
    if let Some(event_types) = params.event_types.as_ref() {
        events.retain(|event| {
            event_types
                .iter()
                .any(|event_type| event_type == event.event.event_name())
        });
    }
    if let Some(categories) = params.categories.as_ref() {
        events.retain(|event| {
            let category = event
                .event
                .event_name()
                .split('.')
                .next()
                .unwrap_or_default();
            categories.iter().any(|candidate| candidate == category)
        });
    }
    if let Some(cutoff) = created_after {
        events.retain(|event| event.event.ts >= cutoff);
    }
    if let Some(cutoff) = created_before {
        events.retain(|event| event.event.ts <= cutoff);
    }
    if matches!(params.action, RunEventsAction::Search) {
        if let Some(query) = params.query.as_deref() {
            events.retain(|event| {
                serde_json::to_string(event).is_ok_and(|serialized| serialized.contains(query))
            });
        }
    }
}

fn run_event_result(
    event: &EventEnvelope,
    max_content_length: usize,
) -> ToolResult<RunEventResult> {
    let mut serialized = serde_json::to_string(event)
        .map_err(|err| ToolError::message(format!("failed to serialize event: {err}")))?;
    let truncated = serialized.len() > max_content_length;
    let event_value = if truncated {
        serialized.truncate(floor_char_boundary(&serialized, max_content_length));
        Value::String(serialized)
    } else {
        serde_json::to_value(event)
            .map_err(|err| ToolError::message(format!("failed to serialize event: {err}")))?
    };
    Ok(RunEventResult {
        event_id: event.event.id.clone(),
        sequence: event.seq,
        event: event_value,
        truncated,
    })
}

fn floor_char_boundary(value: &str, max_len: usize) -> usize {
    let mut boundary = max_len.min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use fabro_types::{EventBody, EventEnvelope, RunEvent, fixtures};
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn run_event_result_truncates_at_utf8_boundary() {
        let event = EventEnvelope {
            seq:   1,
            event: RunEvent {
                id:                 "evt_utf8".to_string(),
                ts:                 Utc::now(),
                run_id:             fixtures::RUN_1,
                node_id:            None,
                node_label:         None,
                stage_id:           None,
                parallel_group_id:  None,
                parallel_branch_id: None,
                session_id:         None,
                parent_session_id:  None,
                tool_call_id:       None,
                actor:              None,
                body:               EventBody::Unknown {
                    name:       "test.utf8".to_string(),
                    properties: json!({ "message": "éééé" }),
                },
            },
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let first_multibyte = serialized
            .find('é')
            .expect("serialized event should contain é");

        let result = run_event_result(&event, first_multibyte + 1).unwrap();

        assert!(result.truncated);
        let Value::String(event_json) = result.event else {
            panic!("truncated events should return string payloads");
        };
        assert!(event_json.is_char_boundary(event_json.len()));
    }
}
