use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use fabro_llm::generate::{stream as llm_stream, GenerateParams};
use fabro_llm::types::{Message as LlmMessage, StreamEvent};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::error::ApiError;
use crate::jwt_auth::AuthenticatedService;
use crate::server::{AppState, PaginationParams};

pub type SessionStore = Arc<RwLock<HashMap<uuid::Uuid, SessionState>>>;

pub fn new_session_store() -> SessionStore {
    Arc::new(RwLock::new(HashMap::new()))
}

pub struct SessionState {
    pub id: uuid::Uuid,
    pub title: String,
    pub model_id: String,
    pub model_provider: Option<String>,
    pub system_prompt: Option<String>,
    pub turns: Vec<fabro_api_types::SessionTurn>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub event_tx: broadcast::Sender<SessionEvent>,
    pub generation_seq: Arc<AtomicU64>,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    TextDelta {
        delta: String,
    },
    AssistantTurnComplete {
        content: String,
        created_at: chrono::DateTime<chrono::Utc>,
    },
    Done,
    Error {
        message: String,
    },
}

fn generate_title(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.len() <= 60 {
        return trimmed.to_string();
    }
    // Find a word boundary near 60 chars
    match trimmed[..60].rfind(' ') {
        Some(pos) => format!("{}…", &trimmed[..pos]),
        None => format!("{}…", &trimmed[..60]),
    }
}

fn resolve_model(model_arg: Option<String>) -> (String, Option<String>) {
    let raw = model_arg.unwrap_or_else(|| {
        fabro_model::Catalog::builtin()
            .list(None)
            .first()
            .map_or_else(|| "claude-sonnet-4-5".to_string(), |m| m.id.clone())
    });
    match fabro_model::Catalog::builtin().get(&raw) {
        Some(info) => (info.id.clone(), Some(info.provider.to_string())),
        None => (raw, None),
    }
}

fn turns_to_messages(turns: &[fabro_api_types::SessionTurn]) -> Vec<LlmMessage> {
    turns
        .iter()
        .filter_map(|turn| match turn {
            fabro_api_types::SessionTurn::UserTurn(t) => Some(LlmMessage::user(&t.content)),
            fabro_api_types::SessionTurn::AssistantTurn(t) => {
                Some(LlmMessage::assistant(&t.content))
            }
            fabro_api_types::SessionTurn::ToolTurn(_) => None,
        })
        .collect()
}

fn spawn_generation(store: SessionStore, session_id: uuid::Uuid, dry_run: bool, seq_at_start: u64) {
    tokio::spawn(async move {
        let (event_tx, model_id, model_provider, system_prompt, messages, generation_seq) = {
            let store = store.read().expect("session store lock poisoned");
            let session = match store.get(&session_id) {
                Some(s) => s,
                None => return,
            };
            (
                session.event_tx.clone(),
                session.model_id.clone(),
                session.model_provider.clone(),
                session.system_prompt.clone(),
                turns_to_messages(&session.turns),
                Arc::clone(&session.generation_seq),
            )
        };

        if dry_run {
            let content = "This is a dry-run response.".to_string();
            let now = chrono::Utc::now();
            let _ = event_tx.send(SessionEvent::TextDelta {
                delta: content.clone(),
            });
            let _ = event_tx.send(SessionEvent::AssistantTurnComplete {
                content: content.clone(),
                created_at: now,
            });

            // Append assistant turn to session
            {
                let mut store = store.write().expect("session store lock poisoned");
                if let Some(session) = store.get_mut(&session_id) {
                    session
                        .turns
                        .push(fabro_api_types::SessionTurn::AssistantTurn(
                            fabro_api_types::AssistantTurn {
                                kind: fabro_api_types::AssistantTurnKind::Assistant,
                                content,
                                created_at: now,
                            },
                        ));
                    session.updated_at = now;
                }
            }
            let _ = event_tx.send(SessionEvent::Done);
            return;
        }

        let mut params = GenerateParams::new(&model_id)
            .messages(messages)
            .max_tokens(4096);
        if let Some(ref provider) = model_provider {
            params = params.provider(provider);
        }
        if let Some(ref system) = system_prompt {
            params = params.system(system);
        }

        let stream_result = match llm_stream(params).await {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx.send(SessionEvent::Error {
                    message: format!("LLM error: {e}"),
                });
                return;
            }
        };

        use futures_util::StreamExt;
        let mut stream_result = stream_result;
        let mut full_text = String::new();
        while let Some(event) = stream_result.next().await {
            // Check if generation was superseded by a new message
            if generation_seq.load(Ordering::Relaxed) != seq_at_start {
                return;
            }
            match event {
                Ok(StreamEvent::TextDelta { delta, .. }) => {
                    full_text.push_str(&delta);
                    let _ = event_tx.send(SessionEvent::TextDelta { delta });
                }
                Err(e) => {
                    let _ = event_tx.send(SessionEvent::Error {
                        message: format!("Stream error: {e}"),
                    });
                    return;
                }
                _ => {}
            }
        }

        let now = chrono::Utc::now();
        let _ = event_tx.send(SessionEvent::AssistantTurnComplete {
            content: full_text.clone(),
            created_at: now,
        });

        // Append assistant turn to session
        {
            let mut store = store.write().expect("session store lock poisoned");
            if let Some(session) = store.get_mut(&session_id) {
                session
                    .turns
                    .push(fabro_api_types::SessionTurn::AssistantTurn(
                        fabro_api_types::AssistantTurn {
                            kind: fabro_api_types::AssistantTurnKind::Assistant,
                            content: full_text,
                            created_at: now,
                        },
                    ));
                session.updated_at = now;
            }
        }
        let _ = event_tx.send(SessionEvent::Done);
    });
}

pub async fn create_session(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Json(req): Json<fabro_api_types::CreateSessionRequest>,
) -> Response {
    let (model_id, model_provider) = resolve_model(req.model);
    let now = chrono::Utc::now();
    let session_id = uuid::Uuid::new_v4();
    let title = generate_title(&req.content);

    let (event_tx, _) = broadcast::channel(256);
    let generation_seq = Arc::new(AtomicU64::new(1));

    let user_turn = fabro_api_types::SessionTurn::UserTurn(fabro_api_types::UserTurn {
        kind: fabro_api_types::UserTurnKind::User,
        content: req.content,
        created_at: now,
    });

    let session = SessionState {
        id: session_id,
        title: title.clone(),
        model_id: model_id.clone(),
        model_provider: model_provider.clone(),
        system_prompt: req.system,
        turns: vec![user_turn],
        created_at: now,
        updated_at: now,
        event_tx: event_tx.clone(),
        generation_seq: Arc::clone(&generation_seq),
    };

    {
        let mut store = state.sessions.write().expect("session store lock poisoned");
        store.insert(session_id, session);
    }

    spawn_generation(Arc::clone(&state.sessions), session_id, state.dry_run, 1);

    (
        StatusCode::CREATED,
        Json(fabro_api_types::CreateSessionResponse {
            id: session_id,
            title,
            model: fabro_api_types::ModelReference { id: model_id },
            created_at: now,
            updated_at: now,
        }),
    )
        .into_response()
}

pub async fn retrieve_session(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let store = state.sessions.read().expect("session store lock poisoned");
    match store.get(&id) {
        Some(session) => (
            StatusCode::OK,
            Json(fabro_api_types::SessionDetail {
                id: session.id,
                title: session.title.clone(),
                model: fabro_api_types::ModelReference {
                    id: session.model_id.clone(),
                },
                created_at: session.created_at,
                updated_at: session.updated_at,
                turns: session.turns.clone(),
            }),
        )
            .into_response(),
        None => ApiError::not_found("Session not found.").into_response(),
    }
}

pub async fn send_message(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Json(req): Json<fabro_api_types::SendMessageRequest>,
) -> Response {
    let seq = {
        let mut store = state.sessions.write().expect("session store lock poisoned");
        match store.get_mut(&id) {
            Some(session) => {
                let now = chrono::Utc::now();
                session.turns.push(fabro_api_types::SessionTurn::UserTurn(
                    fabro_api_types::UserTurn {
                        kind: fabro_api_types::UserTurnKind::User,
                        content: req.content,
                        created_at: now,
                    },
                ));
                session.updated_at = now;
                session.generation_seq.fetch_add(1, Ordering::Relaxed) + 1
            }
            None => return ApiError::not_found("Session not found.").into_response(),
        }
    };

    spawn_generation(Arc::clone(&state.sessions), id, state.dry_run, seq);

    (
        StatusCode::ACCEPTED,
        Json(fabro_api_types::SendMessageResponse { accepted: true }),
    )
        .into_response()
}

pub async fn stream_session_events(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let rx = {
        let store = state.sessions.read().expect("session store lock poisoned");
        match store.get(&id) {
            Some(session) => session.event_tx.subscribe(),
            None => return ApiError::not_found("Session not found.").into_response(),
        }
    };

    use tokio_stream::StreamExt;

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let sse: Option<Event> = match event {
                SessionEvent::TextDelta { delta } => Some(
                    Event::default()
                        .event("content_delta")
                        .data(serde_json::json!({"delta": delta}).to_string()),
                ),
                SessionEvent::AssistantTurnComplete {
                    content,
                    created_at,
                } => Some(
                    Event::default().event("assistant_turn").data(
                        serde_json::json!({
                            "kind": "assistant",
                            "content": content,
                            "created_at": created_at,
                        })
                        .to_string(),
                    ),
                ),
                SessionEvent::Done => Some(Event::default().event("done").data("{}")),
                SessionEvent::Error { message } => Some(
                    Event::default()
                        .event("error")
                        .data(serde_json::json!({"message": message}).to_string()),
                ),
            };
            sse.map(Ok::<_, std::convert::Infallible>)
        }
        Err(_) => None,
    });

    Sse::new(stream).into_response()
}

pub async fn list_sessions(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let store = state.sessions.read().expect("session store lock poisoned");
    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;

    let mut items: Vec<fabro_api_types::SessionListItem> = store
        .values()
        .map(|session| {
            let last_message_preview = session
                .turns
                .last()
                .map(|t| match t {
                    fabro_api_types::SessionTurn::UserTurn(u) => u.content.clone(),
                    fabro_api_types::SessionTurn::AssistantTurn(a) => a.content.clone(),
                    fabro_api_types::SessionTurn::ToolTurn(_) => String::new(),
                })
                .unwrap_or_default();
            let preview = if last_message_preview.len() > 100 {
                format!("{}…", &last_message_preview[..100])
            } else {
                last_message_preview
            };
            fabro_api_types::SessionListItem {
                id: session.id,
                title: session.title.clone(),
                model: fabro_api_types::ModelReference {
                    id: session.model_id.clone(),
                },
                last_message_preview: preview,
                created_at: session.created_at,
                updated_at: session.updated_at,
            }
        })
        .collect();

    // Sort by updated_at desc
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let page: Vec<_> = items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = page.len() > limit;
    let data: Vec<_> = page.into_iter().take(limit).collect();

    (
        StatusCode::OK,
        Json(fabro_api_types::PaginatedSessionList {
            data,
            meta: fabro_api_types::PaginationMeta { has_more },
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::jwt_auth::AuthMode;
    use crate::server::{build_router, create_app_state_with_options};

    use fabro_workflows::pipeline::LlmSpec;

    fn test_llm_spec() -> LlmSpec {
        LlmSpec {
            model: "test-model".to_string(),
            provider: fabro_llm::Provider::Anthropic,
            fallback_chain: Vec::new(),
            mcp_servers: Vec::new(),
            dry_run: true,
        }
    }

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    async fn dry_run_app() -> axum::Router {
        let db = test_db().await;
        let state = create_app_state_with_options(
            db,
            test_llm_spec,
            true,
            5,
            fabro_workflows::git::GitAuthor::default(),
            Vec::new(),
        );
        build_router(state, AuthMode::Disabled)
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn create_test_session(app: &axum::Router) -> serde_json::Value {
        let req = Request::builder()
            .method("POST")
            .uri("/sessions")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "content": "Hello, world!"
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        body_json(response.into_body()).await
    }

    #[tokio::test]
    async fn create_session_returns_201() {
        let app = dry_run_app().await;
        let body = create_test_session(&app).await;

        assert!(body["id"].is_string());
        assert!(body["title"].is_string());
        assert!(body["model"]["id"].is_string());
        assert!(body["created_at"].is_string());
        assert!(body["updated_at"].is_string());
    }

    #[tokio::test]
    async fn retrieve_session_after_create() {
        let app = dry_run_app().await;
        let create_body = create_test_session(&app).await;
        let session_id = create_body["id"].as_str().unwrap();

        // Give generation task a moment to complete
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["id"].as_str().unwrap(), session_id);
        assert!(body["turns"].is_array());
        // Should have at least the initial user turn
        assert!(!body["turns"].as_array().unwrap().is_empty());
        assert_eq!(body["turns"][0]["kind"], "user");
    }

    #[tokio::test]
    async fn retrieve_session_not_found() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("GET")
            .uri("/sessions/a1b2c3d4-e5f6-7890-abcd-ef1234567890")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn send_message_returns_202() {
        let app = dry_run_app().await;
        let create_body = create_test_session(&app).await;
        let session_id = create_body["id"].as_str().unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/sessions/{session_id}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "content": "Follow up question"
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["accepted"], true);
    }

    #[tokio::test]
    async fn send_message_not_found() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("POST")
            .uri("/sessions/a1b2c3d4-e5f6-7890-abcd-ef1234567890/messages")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "content": "Hello"
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_sessions_empty() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("GET")
            .uri("/sessions")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert!(body["data"].as_array().unwrap().is_empty());
        assert_eq!(body["meta"]["has_more"], false);
    }

    #[tokio::test]
    async fn list_sessions_after_create() {
        let app = dry_run_app().await;
        let _create_body = create_test_session(&app).await;

        let req = Request::builder()
            .method("GET")
            .uri("/sessions")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stream_events_dry_run() {
        let app = dry_run_app().await;
        let create_body = create_test_session(&app).await;
        let session_id = create_body["id"].as_str().unwrap();

        // Give the generation task a moment to produce events
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/sessions/{session_id}/events"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/event-stream"
        );
    }
}
