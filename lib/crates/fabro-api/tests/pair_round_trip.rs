use std::any::{TypeId, type_name};

use fabro_api::types::{
    PairId as ApiPairId, PairMessageId as ApiPairMessageId,
    PairMessageRecord as ApiPairMessageRecord, PairMessageRequest as ApiPairMessageRequest,
    PairRecord as ApiPairRecord, PairStartRequest as ApiPairStartRequest,
    PairStatus as ApiPairStatus, PairTarget as ApiPairTarget,
    PairTranscriptEntry as ApiPairTranscriptEntry,
    PairTranscriptResponse as ApiPairTranscriptResponse,
    RunEventDetailResponse as ApiRunEventDetailResponse,
    RunPairStatusResponse as ApiRunPairStatusResponse,
};
use fabro_types::{
    PairId, PairMessageId, PairMessageRecord, PairMessageRequest, PairRecord, PairStartRequest,
    PairStatus, PairTarget, PairTranscriptEntry, PairTranscriptResponse, RunEventDetailResponse,
    RunPairStatusResponse, fixtures,
};
use serde_json::{Value, json};

#[test]
fn pair_api_reuses_canonical_types() {
    assert_same_type::<ApiPairId, PairId>();
    assert_same_type::<ApiPairMessageId, PairMessageId>();
    assert_same_type::<ApiPairStatus, PairStatus>();
    assert_same_type::<ApiPairTarget, PairTarget>();
    assert_same_type::<ApiPairRecord, PairRecord>();
    assert_same_type::<ApiRunPairStatusResponse, RunPairStatusResponse>();
    assert_same_type::<ApiPairStartRequest, PairStartRequest>();
    assert_same_type::<ApiPairMessageRequest, PairMessageRequest>();
    assert_same_type::<ApiPairMessageRecord, PairMessageRecord>();
    assert_same_type::<ApiPairTranscriptEntry, PairTranscriptEntry>();
    assert_same_type::<ApiPairTranscriptResponse, PairTranscriptResponse>();
    assert_same_type::<ApiRunEventDetailResponse, RunEventDetailResponse>();
}

#[test]
fn pair_target_omits_internal_fields() {
    let target = PairTarget {
        stage_id:   "code@1".parse().unwrap(),
        node_label: "Code".to_string(),
    };
    let serialized = serde_json::to_value(&target).unwrap();
    assert_eq!(
        serialized,
        json!({
            "stage_id": "code@1",
            "node_label": "Code"
        })
    );
    assert!(serialized.get("agent_session_id").is_none());
    assert!(serialized.get("session_id").is_none());
    assert!(serialized.get("node_id").is_none());
    assert!(serialized.get("visit").is_none());
    assert!(serialized.get("provider").is_none());
    assert!(serialized.get("model").is_none());
}

#[test]
fn pair_start_request_only_carries_stage_id() {
    let request = PairStartRequest {
        stage_id: "code@1".parse().unwrap(),
    };
    let serialized = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serialized,
        json!({
            "stage_id": "code@1"
        })
    );
    assert!(serialized.get("agent_session_id").is_none());
    assert!(serialized.get("target").is_none());
}

#[test]
fn pair_record_round_trips_json() {
    assert_round_trip::<PairRecord>(json!({
        "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
        "run_id": fixtures::RUN_1,
        "status": "active",
        "started_at": "2026-05-18T12:00:01Z",
        "ended_at": null,
        "failure_reason": null,
        "target": pair_target_json()
    }));
}

#[test]
fn pair_message_record_round_trips_json() {
    assert_round_trip::<PairMessageRecord>(json!({
        "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
        "client_message_id": "client-1",
        "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
        "run_id": fixtures::RUN_1,
        "stage_id": "code@1",
        "text": "Can you inspect the failing test?",
        "accepted_at": "2026-05-18T12:01:00Z"
    }));
}

#[test]
fn pair_status_response_round_trips_json() {
    assert_round_trip::<RunPairStatusResponse>(json!({
        "run_id": fixtures::RUN_1,
        "current_pair": null,
        "targets": [pair_target_json()]
    }));
}

#[test]
fn pair_transcript_response_round_trips_json() {
    assert_round_trip::<PairTranscriptResponse>(json!({
        "data": [
            {
                "kind": "user_message",
                "seq": 42,
                "event_id": "evt_1",
                "ts": "2026-05-18T12:01:00Z",
                "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
                "target": pair_target_json(),
                "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
                "client_message_id": "client-1",
                "text": "Can you inspect the failing test?"
            },
            {
                "kind": "system_message",
                "seq": 43,
                "event_id": "evt_2",
                "ts": "2026-05-18T12:01:01Z",
                "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
                "target": pair_target_json(),
                "system_message_kind": "human_joined",
                "text": "A human has joined this workflow run for live pairing. Wait for their next message before continuing."
            }
        ],
        "meta": {
            "next_since_seq": 44,
            "has_more": false
        }
    }));
}

#[test]
fn run_event_detail_response_round_trips_json() {
    assert_round_trip::<RunEventDetailResponse>(json!({
        "event": {
            "seq": 45,
            "id": "evt_3",
            "ts": "2026-05-18T12:01:20Z",
            "run_id": fixtures::RUN_1,
            "event": "agent.tool.completed",
            "session_id": "ses_01",
            "node_id": "code",
            "node_label": "Code",
            "stage_id": "code@1",
            "tool_call_id": "call_7"
        },
        "properties": {
            "tool_name": "shell",
            "tool_call_id": "call_7",
            "is_error": false,
            "visit": 1
        },
        "content": {
            "kind": "tool_output",
            "value": "..."
        },
        "truncated": false,
        "redacted": false,
        "max_content_length": 20000
    }));
}

fn pair_target_json() -> Value {
    json!({
        "stage_id": "code@1",
        "node_label": "Code"
    })
}

fn assert_round_trip<T>(value: Value)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let parsed: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
