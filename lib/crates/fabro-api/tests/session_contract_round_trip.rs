use std::any::{TypeId, type_name};

use chrono::{TimeZone, Utc};
use fabro_api::types::{
    SessionDetail as ApiSessionDetail, SessionRecord as ApiSessionRecord,
    SessionSummary as ApiSessionSummary, SessionTurn as ApiSessionTurn, SubmitTurnRequest,
};
use fabro_types::{
    SessionDetail, SessionId, SessionMessage, SessionRecord, SessionStatus, SessionSummary,
    SessionTurn, TurnId, fixtures,
};
use serde_json::json;

#[test]
fn session_contract_reuses_domain_types() {
    assert_same_type::<ApiSessionTurn, SessionTurn>();
    assert_same_type::<ApiSessionRecord, SessionRecord>();
    assert_same_type::<ApiSessionSummary, SessionSummary>();
    assert_same_type::<ApiSessionDetail, SessionDetail>();
}

#[test]
fn session_detail_round_trips_messages_active_turn_and_last_seq() {
    let created_at = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
    let turn_started_at = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 1).unwrap();
    let updated_at = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 2).unwrap();
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let detail = SessionDetail::new(
        SessionRecord {
            id: session_id,
            run_id: fixtures::RUN_1,
            title: Some("Ask Fabro".to_string()),
            status: SessionStatus::Running,
            model: Some("gpt-5.4".to_string()),
            active_turn: Some(SessionTurn {
                id:         turn_id,
                started_at: turn_started_at,
                input:      "What changed?".to_string(),
            }),
            created_at,
            updated_at,
        },
        vec![SessionMessage::user("What changed?", updated_at)],
        7,
    );

    let value = serde_json::to_value(&detail).expect("detail should serialize");
    assert_eq!(value["active_turn"]["id"], turn_id.to_string());
    assert_eq!(value["messages"][0]["kind"], "user");
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
