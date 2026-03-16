use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::event::Track;

const SEGMENT_API_URL: &str = "https://api.segment.io/v1/track";
const SEGMENT_WRITE_KEY: Option<&str> = option_env!("SEGMENT_WRITE_KEY");

/// Serializes the track event to a temp file and spawns a detached subprocess
/// (`fabro __send_analytics <path>`) to deliver it. This ensures the event is
/// sent even if the parent CLI process exits immediately.
///
/// No-ops if the SEGMENT_WRITE_KEY was not set at compile time.
pub fn send(track: Track) {
    if SEGMENT_WRITE_KEY.is_none() {
        tracing::debug!("telemetry: no SEGMENT_WRITE_KEY, skipping send");
        return;
    }

    spawn_sender(track);
}

fn spawn_sender(track: Track) {
    let json = match serde_json::to_vec(&track) {
        Ok(j) => j,
        Err(_) => return,
    };

    let filename = format!("fabro-event-{}.json", track.message_id);
    super::spawn::spawn_fabro_subcommand("__send_analytics", &filename, &json);
}

/// Reads a serialized track event from `path` and sends it to Segment.
/// Called by the `__send_analytics` subcommand.
/// No-ops if `SEGMENT_WRITE_KEY` was not set at compile time.
pub async fn send_to_segment(path: &Path) -> anyhow::Result<()> {
    let write_key = SEGMENT_WRITE_KEY
        .ok_or_else(|| anyhow::anyhow!("SEGMENT_WRITE_KEY not set at compile time"))?;

    let json = std::fs::read(path)?;
    let track: Track = serde_json::from_slice(&json)?;

    let auth = STANDARD.encode(format!("{write_key}:"));

    let resp = reqwest::Client::new()
        .post(SEGMENT_API_URL)
        .header("Authorization", format!("Basic {auth}"))
        .json(&track)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("segment API returned status {}", resp.status());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::event::User;
    use serde_json::json;

    #[test]
    fn send_noops_without_write_key() {
        // SEGMENT_WRITE_KEY is not set at compile time in tests,
        // so send() should return immediately without spawning.
        let track = Track {
            user: User::AnonymousId {
                anonymous_id: "test".to_string(),
            },
            event: "test".to_string(),
            properties: json!({}),
            context: None,
            timestamp: None,
            message_id: "msg-test".to_string(),
        };

        // This should not panic or require a tokio runtime
        // because it returns before reaching tokio::spawn
        send(track);
    }
}
