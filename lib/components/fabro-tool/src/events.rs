use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::RunStreamItem;
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

/// The `fabro_run_events` tool's parameters: which page of a run's stream
/// to read (`after` is the last `stream_seq` seen, exclusive) and how to
/// narrow it.
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
    pub after:              Option<u64>,
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
    /// The `after` cursor for the next page: the last `stream_seq` returned.
    pub next_cursor: Option<u64>,
}

/// One item of the run's stream, as the tool returns it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RunEventResult {
    pub event_id:  String,
    pub sequence:  u64,
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
    let fetch_after = if descending {
        0
    } else {
        raw.after.unwrap_or(0)
    };
    let mut items = backend
        .list_run_stream(&run_id, fetch_after, event_fetch_limit(&raw, first))
        .await
        .map_err(|err| ToolError::from_anyhow(&err))?;
    if descending {
        if let Some(after) = raw.after {
            items.retain(|item| item.stream_seq < after);
        }
    }
    filter_items(&mut items, &raw, created_after, created_before);
    if descending {
        items.reverse();
    }
    let offset = raw.offset.unwrap_or(0);
    let page = items
        .into_iter()
        .skip(offset)
        .take(first)
        .collect::<Vec<_>>();
    let max_content_length = raw.max_content_length.unwrap_or(20_000);
    let results = page
        .iter()
        .map(|item| run_event_result(item, max_content_length))
        .collect::<ToolResult<Vec<_>>>()?;
    let next_cursor = page.last().map(|item| item.stream_seq);

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

/// How many items to read from the server: the page plus its offset when
/// the request is a plain ascending page, the whole stream when a filter,
/// a search or descending order needs every item.
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

fn filter_items(
    items: &mut Vec<RunStreamItem>,
    params: &FabroRunEventsParams,
    created_after: Option<DateTime<Utc>>,
    created_before: Option<DateTime<Utc>>,
) {
    if let Some(event_ids) = params.event_ids.as_ref() {
        items.retain(|item| event_ids.contains(&item.id));
    }
    if let Some(event_types) = params.event_types.as_ref() {
        items.retain(|item| {
            item.name()
                .is_some_and(|name| event_types.iter().any(|event_type| event_type == name))
        });
    }
    if let Some(categories) = params.categories.as_ref() {
        items.retain(|item| {
            let category = item
                .name()
                .and_then(|name| name.split('.').next())
                .unwrap_or_default();
            categories.iter().any(|candidate| candidate == category)
        });
    }
    if let Some(cutoff) = created_after {
        items.retain(|item| recorded_at(item) >= cutoff);
    }
    if let Some(cutoff) = created_before {
        items.retain(|item| recorded_at(item) <= cutoff);
    }
    if matches!(params.action, RunEventsAction::Search) {
        if let Some(query) = params.query.as_deref() {
            items.retain(|item| {
                serde_json::to_string(item).is_ok_and(|serialized| serialized.contains(query))
            });
        }
    }
}

/// When the item's record was appended, from its epoch milliseconds; the
/// epoch itself for a timestamp outside `DateTime`'s range.
fn recorded_at(item: &RunStreamItem) -> DateTime<Utc> {
    i64::try_from(item.recorded_at)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_default()
}

fn run_event_result(item: &RunStreamItem, max_content_length: usize) -> ToolResult<RunEventResult> {
    let mut serialized = serde_json::to_string(item)
        .map_err(|err| ToolError::message(format!("failed to serialize event: {err}")))?;
    let truncated = serialized.len() > max_content_length;
    let event_value = if truncated {
        serialized.truncate(floor_char_boundary(&serialized, max_content_length));
        Value::String(serialized)
    } else {
        serde_json::to_value(item)
            .map_err(|err| ToolError::message(format!("failed to serialize event: {err}")))?
    };
    Ok(RunEventResult {
        event_id: item.id.clone(),
        sequence: item.stream_seq,
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
    use fabro_types::{RunStreamItemKind, fixtures};
    use serde_json::{Value, json};

    use super::*;

    fn item(stream_seq: u64, name: &str, recorded_at: u64) -> RunStreamItem {
        RunStreamItem {
            run_id: fixtures::RUN_1,
            stream_seq,
            kind: RunStreamItemKind::Petri,
            id: format!("coordinator/{stream_seq}/0"),
            recorded_at,
            item: json!({
                "record": { "body": { "event": name, "message": "éééé" } }
            }),
        }
    }

    #[test]
    fn run_event_result_truncates_at_utf8_boundary() {
        let item = item(1, "test.utf8", 1_789_323_217_366);
        let serialized = serde_json::to_string(&item).unwrap();
        let first_multibyte = serialized
            .find('é')
            .expect("serialized event should contain é");

        let result = run_event_result(&item, first_multibyte + 1).unwrap();

        assert!(result.truncated);
        let Value::String(event_json) = result.event else {
            panic!("truncated events should return string payloads");
        };
        assert!(event_json.is_char_boundary(event_json.len()));
    }

    #[test]
    fn filters_narrow_by_name_category_and_time() {
        let params = FabroRunEventsParams {
            action:             RunEventsAction::List,
            run_id:             fixtures::RUN_1.to_string(),
            event_types:        Some(vec!["stage.started".to_string()]),
            categories:         None,
            direction:          None,
            created_after:      None,
            created_before:     None,
            first:              None,
            after:              None,
            event_ids:          None,
            offset:             None,
            limit:              None,
            max_content_length: None,
            query:              None,
        };
        let mut items = vec![
            item(1, "run.started", 1_000),
            item(2, "stage.started", 2_000),
            item(3, "stage.finished", 3_000),
        ];
        filter_items(&mut items, &params, None, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].stream_seq, 2);

        let params = FabroRunEventsParams {
            event_types: None,
            categories: Some(vec!["stage".to_string()]),
            ..params
        };
        let mut items = vec![
            item(1, "run.started", 1_000),
            item(2, "stage.started", 2_000),
            item(3, "stage.finished", 3_000),
        ];
        filter_items(
            &mut items,
            &params,
            DateTime::from_timestamp_millis(2_500),
            None,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].stream_seq, 3);
    }
}
