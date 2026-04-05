use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fabro_store::{EventEnvelope, EventPayload, SlateRunStore, SlateStore};
use object_store::memory::InMemory;

pub(crate) async fn rebuild_run_store(
    run_id: &fabro_types::RunId,
    events: &[EventEnvelope],
) -> Result<SlateRunStore> {
    let store = Arc::new(SlateStore::new(
        Arc::new(InMemory::new()),
        "",
        Duration::from_millis(1),
    ));
    let run_store = store.create_run(run_id).await?;
    for event in events {
        let payload = EventPayload::new(event.payload.as_value().clone(), run_id)?;
        run_store.append_event(&payload).await?;
    }
    Ok(run_store)
}
