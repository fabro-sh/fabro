use chrono::{DateTime, Utc};

mod artifact_store;
mod error;
mod keyed_mutex;
mod keys;
mod record;
mod run_state;
mod serializable_projection;
mod session_store;
mod slate;
mod types;

pub use artifact_store::{
    ArtifactKey, ArtifactStore, NodeArtifact, StageArtifactEntry, retry_storage_segment,
    stage_storage_segment,
};
pub use error::{Error, Result};
pub use fabro_types::{
    EventEnvelope, PendingInterviewRecord, Run, RunBlobId, RunProjection, StageId, StageProjection,
};
pub(crate) use keyed_mutex::KeyedMutex;
pub use run_state::RunProjectionReducer;
pub use serializable_projection::SerializableProjection;
pub use session_store::SessionStore;
pub use slate::{
    AuthCode, AuthCodeStore, Blob, BlobStore, CachedRunProjection, ConsumeOutcome, Database,
    RefreshToken, RefreshTokenStore, RunCatalogIndex, RunDatabase, Runs, UnreadableRun,
};
pub use types::EventPayload;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListRunsQuery {
    pub start:     Option<DateTime<Utc>>,
    pub end:       Option<DateTime<Utc>>,
    pub parent_id: Option<fabro_types::RunId>,
}

#[cfg(test)]
mod session_store_contract_tests {
    use chrono::Utc;
    use fabro_types::{
        SessionEventEnvelope, SessionId, SessionRecord, SessionStatus, TurnId, TurnRecord,
        TurnStatus,
    };
    use serde_json::json;

    use crate::SessionStore;

    #[tokio::test]
    async fn create_list_read_delete_sessions_and_replay_events() {
        let root = tempfile::tempdir().expect("temp store root should exist");
        let store = SessionStore::new(root.path().join("sessions"));
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let now = Utc::now();
        let mut session = SessionRecord::new(session_id, now);
        session.title = Some("Investigate failure".to_string());
        session.working_dir = Some("/tmp/project".to_string());
        session.provider = Some("openai".to_string());
        session.model = Some("gpt-5.4-mini".to_string());

        store
            .create_session(session.clone())
            .await
            .expect("session should persist");
        store
            .append_turn(TurnRecord {
                id: turn_id,
                session_id,
                input: "hello".to_string(),
                status: TurnStatus::Succeeded,
                output: Some("world".to_string()),
                error: None,
                created_at: now,
                updated_at: now,
                completed_at: Some(now),
            })
            .await
            .expect("turn should persist");
        let first = store
            .append_event(SessionEventEnvelope::new(
                session_id,
                Some(turn_id),
                "session.created",
                json!({"title": "Investigate failure"}),
                now,
            ))
            .await
            .expect("first event should persist");
        let second = store
            .append_event(SessionEventEnvelope::new(
                session_id,
                Some(turn_id),
                "turn.completed",
                json!({"ok": true}),
                now,
            ))
            .await
            .expect("second event should persist");

        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(
            store
                .get_session(session_id)
                .await
                .expect("session read should succeed")
                .expect("session should exist")
                .working_dir
                .as_deref(),
            Some("/tmp/project")
        );
        assert_eq!(
            store
                .list_sessions()
                .await
                .expect("session list should succeed")
                .len(),
            1
        );
        assert_eq!(
            store
                .list_turns(session_id)
                .await
                .expect("turn list should succeed")
                .len(),
            1
        );
        let replayed = store
            .list_events(session_id, Some(2))
            .await
            .expect("event replay should succeed");
        assert_eq!(
            replayed
                .into_iter()
                .map(|event| event.event)
                .collect::<Vec<_>>(),
            vec!["turn.completed".to_string()]
        );

        store
            .delete_session(session_id)
            .await
            .expect("delete should succeed");
        assert!(
            store
                .get_session(session_id)
                .await
                .expect("session read should succeed")
                .is_none()
        );
        assert!(
            store
                .list_sessions()
                .await
                .expect("session list should succeed")
                .is_empty()
        );
    }

    #[test]
    fn session_and_turn_ids_reject_malformed_values() {
        assert!("not-a-ulid".parse::<SessionId>().is_err());
        assert!("not-a-ulid".parse::<TurnId>().is_err());
        assert_eq!(SessionStatus::Idle.as_str(), "idle");
    }

    #[tokio::test]
    async fn stale_running_sessions_and_turns_recover_to_idle_and_interrupted() {
        let root = tempfile::tempdir().expect("temp store root should exist");
        let store = SessionStore::new(root.path().join("sessions"));
        let session_id = SessionId::new();
        let running_turn_id = TurnId::new();
        let now = Utc::now();
        let mut session = SessionRecord::new(session_id, now);
        session.status = SessionStatus::Running;
        store
            .create_session(session)
            .await
            .expect("session should persist");

        store
            .append_turn(TurnRecord {
                id: running_turn_id,
                session_id,
                input: "hello".to_string(),
                status: TurnStatus::Running,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
                completed_at: None,
            })
            .await
            .expect("turn should persist");

        let recovered_at = now + chrono::Duration::seconds(5);
        store
            .recover_stale_running_state(recovered_at)
            .expect("stale runtime state should recover");

        let recovered = store
            .get_session(session_id)
            .await
            .expect("session read should succeed")
            .expect("session should exist");
        assert_eq!(recovered.status, SessionStatus::Idle);
        assert_eq!(recovered.updated_at, recovered_at);

        let turns = store
            .list_turns(session_id)
            .await
            .expect("turn list should succeed");
        assert_eq!(turns.len(), 1);
        for turn in turns {
            assert_eq!(turn.status, TurnStatus::Interrupted);
            assert_eq!(turn.completed_at, Some(recovered_at));
            assert_eq!(
                turn.error.as_deref(),
                Some("Server restarted before the turn completed.")
            );
        }
    }
}
