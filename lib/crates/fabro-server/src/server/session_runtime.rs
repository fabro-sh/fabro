use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fabro_agent::Session;
use fabro_types::{SessionEventEnvelope, SessionId, TurnId};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard, broadcast};
use tokio_util::sync::CancellationToken;

const SESSION_EVENT_BROADCAST_CAPACITY: usize = 1024;

#[derive(Default)]
pub(crate) struct SessionRuntimeManager {
    entries: Mutex<HashMap<SessionId, Arc<SessionRuntimeEntry>>>,
}

impl SessionRuntimeManager {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn load_or_create_runtime(&self, session_id: SessionId) -> Arc<SessionRuntimeEntry> {
        self.entry(session_id)
    }

    pub(crate) fn has_active_turn(&self, session_id: SessionId) -> bool {
        self.existing_entry(session_id)
            .is_some_and(|entry| entry.has_active_turn())
    }

    pub(crate) fn reserve_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<SessionTurnLease, StartTurnError> {
        let entry = self.load_or_create_runtime(session_id);
        {
            let mut active = entry
                .active_turn
                .lock()
                .expect("session active turn lock poisoned");
            if active.is_some() {
                return Err(StartTurnError::ActiveTurn);
            }
            *active = Some(ActiveTurn {
                turn_id,
                cancel_token: None,
                interrupt_requested: false,
            });
        }
        Ok(SessionTurnLease { entry, turn_id })
    }

    pub(crate) fn request_interrupt(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<PendingTurnInterrupt, InterruptTurnError> {
        let Some(entry) = self.existing_entry(session_id) else {
            return Err(InterruptTurnError::NotActive);
        };
        {
            let active = entry
                .active_turn
                .lock()
                .expect("session active turn lock poisoned");
            let Some(active) = active.as_ref() else {
                return Err(InterruptTurnError::NotActive);
            };
            if active.turn_id != turn_id {
                return Err(InterruptTurnError::NotActive);
            }
        }
        Ok(PendingTurnInterrupt { entry, turn_id })
    }

    pub(crate) fn subscribe_events(
        &self,
        session_id: SessionId,
    ) -> broadcast::Receiver<SessionEventEnvelope> {
        self.load_or_create_runtime(session_id).live_tx.subscribe()
    }

    pub(crate) fn broadcast_event(&self, event: &SessionEventEnvelope) {
        let Some(entry) = self.existing_entry(event.session_id) else {
            return;
        };
        let _ = entry.live_tx.send(event.clone());
    }

    pub(crate) async fn unload_idle(&self, session_id: SessionId) -> bool {
        let entry = {
            let mut entries = self.entries.lock().expect("session runtime map poisoned");
            let Some(entry) = entries.get(&session_id) else {
                return true;
            };
            if entry.has_active_turn() {
                return false;
            }
            entries
                .remove(&session_id)
                .expect("session runtime entry should exist")
        };
        entry.clear_session().await;
        true
    }

    fn entry(&self, session_id: SessionId) -> Arc<SessionRuntimeEntry> {
        let mut entries = self.entries.lock().expect("session runtime map poisoned");
        Arc::clone(
            entries
                .entry(session_id)
                .or_insert_with(|| Arc::new(SessionRuntimeEntry::new())),
        )
    }

    fn existing_entry(&self, session_id: SessionId) -> Option<Arc<SessionRuntimeEntry>> {
        self.entries
            .lock()
            .expect("session runtime map poisoned")
            .get(&session_id)
            .cloned()
    }
}

pub(crate) struct SessionRuntimeEntry {
    session:     AsyncMutex<Option<Session>>,
    initialized: Mutex<bool>,
    active_turn: Mutex<Option<ActiveTurn>>,
    live_tx:     broadcast::Sender<SessionEventEnvelope>,
}

impl SessionRuntimeEntry {
    fn new() -> Self {
        let (live_tx, _) = broadcast::channel(SESSION_EVENT_BROADCAST_CAPACITY);
        Self {
            session: AsyncMutex::new(None),
            initialized: Mutex::new(false),
            active_turn: Mutex::new(None),
            live_tx,
        }
    }

    pub(crate) async fn lock_session(&self) -> AsyncMutexGuard<'_, Option<Session>> {
        self.session.lock().await
    }

    pub(crate) fn is_initialized(&self) -> bool {
        *self
            .initialized
            .lock()
            .expect("session initialized lock poisoned")
    }

    pub(crate) fn mark_initialized(&self) {
        *self
            .initialized
            .lock()
            .expect("session initialized lock poisoned") = true;
    }

    pub(crate) async fn clear_session(&self) {
        *self.session.lock().await = None;
        *self
            .initialized
            .lock()
            .expect("session initialized lock poisoned") = false;
    }

    fn has_active_turn(&self) -> bool {
        self.active_turn
            .lock()
            .expect("session active turn lock poisoned")
            .is_some()
    }
}

struct ActiveTurn {
    turn_id:             TurnId,
    cancel_token:        Option<CancellationToken>,
    interrupt_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartTurnError {
    ActiveTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptTurnError {
    NotActive,
}

pub(crate) struct SessionTurnLease {
    entry:   Arc<SessionRuntimeEntry>,
    turn_id: TurnId,
}

pub(crate) struct PendingTurnInterrupt {
    entry:   Arc<SessionRuntimeEntry>,
    turn_id: TurnId,
}

impl PendingTurnInterrupt {
    pub(crate) fn cancel(self) {
        let cancel_token = {
            let mut active = self
                .entry
                .active_turn
                .lock()
                .expect("session active turn lock poisoned");
            let Some(active) = active
                .as_mut()
                .filter(|active| active.turn_id == self.turn_id)
            else {
                return;
            };
            active.interrupt_requested = true;
            active.cancel_token.clone()
        };
        if let Some(cancel_token) = cancel_token {
            cancel_token.cancel();
        }
    }
}

impl SessionTurnLease {
    pub(crate) fn entry(&self) -> Arc<SessionRuntimeEntry> {
        Arc::clone(&self.entry)
    }

    pub(crate) fn attach_cancel_token(&self, cancel_token: &CancellationToken) -> bool {
        let mut active = self
            .entry
            .active_turn
            .lock()
            .expect("session active turn lock poisoned");
        let Some(active) = active
            .as_mut()
            .filter(|active| active.turn_id == self.turn_id)
        else {
            return false;
        };
        active.cancel_token = Some(cancel_token.clone());
        if active.interrupt_requested {
            cancel_token.cancel();
            true
        } else {
            false
        }
    }

    pub(crate) fn interrupt_requested(&self) -> bool {
        self.entry
            .active_turn
            .lock()
            .expect("session active turn lock poisoned")
            .as_ref()
            .is_some_and(|active| active.turn_id == self.turn_id && active.interrupt_requested)
    }
}

impl Drop for SessionTurnLease {
    fn drop(&mut self) {
        let mut active = self
            .entry
            .active_turn
            .lock()
            .expect("session active turn lock poisoned");
        if active
            .as_ref()
            .is_some_and(|active| active.turn_id == self.turn_id)
        {
            *active = None;
        }
    }
}
