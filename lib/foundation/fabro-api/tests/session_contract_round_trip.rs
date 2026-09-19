use std::any::{TypeId, type_name};

use chrono::{TimeZone, Utc};
use fabro_api::types::{
    PaginatedSessionEventList, RunSessionMetadata as ApiRunSessionMetadata,
    SessionDetail as ApiSessionDetail, SessionEvent as ApiSessionEvent,
    SessionSummary as ApiSessionSummary, SessionTurn as ApiSessionTurn, SubmitTurnRequest,
};
use fabro_types::session_event::SessionTurnStartedProps;
use fabro_types::{
    RunSessionMetadata, SessionDetail, SessionEvent, SessionEventBody, SessionId, SessionStatus,
    SessionSummary, SessionTurn, TurnId, fixtures,
};
use serde_json::json;

#[test]
fn session_contract_reuses_domain_types() {
    assert_same_type::<ApiSessionTurn, SessionTurn>();
    assert_same_type::<ApiRunSessionMetadata, RunSessionMetadata>();
    assert_same_type::<ApiSessionSummary, SessionSummary>();
    assert_same_type::<ApiSessionDetail, SessionDetail>();
    assert_same_type::<ApiSessionEvent, SessionEvent>();
}

#[test]
fn a_session_event_page_round_trips_the_flattened_event() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let value = json!({
        "data": [{
            "seq": 2,
            "session_id": session_id.to_string(),
            "run_id": fixtures::RUN_1,
            "ts": "2026-05-20T12:00:01Z",
            "event": "run.session.turn.started",
            "properties": { "turn_id": turn_id.to_string(), "input": "What changed?" }
        }],
        "meta": { "has_more": false }
    });

    let page: PaginatedSessionEventList =
        serde_json::from_value(value.clone()).expect("page should deserialize");
    assert_eq!(
        page.data[0].body,
        SessionEventBody::TurnStarted(SessionTurnStartedProps {
            turn_id,
            input: "What changed?".to_string(),
        })
    );
    assert_eq!(serde_json::to_value(&page).unwrap(), value);
}

#[test]
fn session_detail_round_trips_active_turn_and_last_seq() {
    let created_at = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
    let turn_started_at = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 1).unwrap();
    let updated_at = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 2).unwrap();
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let detail = SessionDetail::new(
        RunSessionMetadata {
            id: session_id,
            run_id: fixtures::RUN_1,
            title: Some("Ask Fabro".to_string()),
            status: SessionStatus::Running,
            model: Some("gpt-5.4".to_string()),
            provider: Some(lithos_llm::catalog::builtin::openai()),
            active_turn: Some(SessionTurn {
                id:         turn_id,
                started_at: turn_started_at,
                input:      "What changed?".to_string(),
            }),
            created_at,
            updated_at,
        },
        7,
    );

    let value = serde_json::to_value(&detail).expect("detail should serialize");
    assert_eq!(value["active_turn"]["id"], turn_id.to_string());
    assert!(
        value.get("messages").is_none(),
        "the conversation stays in the server's session record"
    );
    assert_eq!(value["last_seq"], 7);

    let round_trip: ApiSessionDetail =
        serde_json::from_value(value.clone()).expect("detail should deserialize");
    assert_eq!(serde_json::to_value(round_trip).unwrap(), value);
}

#[test]
fn submit_turn_request_accepts_client_turn_id() {
    let turn_id = TurnId::new();
    let request: SubmitTurnRequest = serde_json::from_value(json!({
        "input": "Summarize this run",
        "turn_id": turn_id.to_string()
    }))
    .expect("submit turn request should deserialize");

    assert_eq!(request.input, "Summarize this run");
    assert_eq!(request.turn_id, Some(turn_id));
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
