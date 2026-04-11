use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::blocking::Client as BlockingClient;
use uuid::Uuid;

use crate::event::Track;
use crate::spawn::spawn_fabro_subcommand;

const SEGMENT_BASE_URL: &str = match option_env!("SEGMENT_BASE_URL") {
    Some(url) => url,
    None => "https://api.segment.io",
};
const SEGMENT_WRITE_KEY: Option<&str> = option_env!("SEGMENT_WRITE_KEY");

/// Serializes the track events as JSONL to a temp file and spawns a detached
/// subprocess (`fabro __send_analytics <path>`) to deliver them. This ensures
/// the events are sent even if the parent CLI process exits immediately.
///
/// No-ops if the SEGMENT_WRITE_KEY was not set at compile time or `tracks` is
/// empty.
pub fn emit(tracks: &[Track]) {
    if SEGMENT_WRITE_KEY.is_none() {
        tracing::debug!("telemetry: no SEGMENT_WRITE_KEY, skipping emit");
        return;
    }

    if tracks.is_empty() {
        return;
    }

    spawn_sender(tracks);
}

fn spawn_sender(tracks: &[Track]) {
    let lines: Vec<String> = tracks
        .iter()
        .filter_map(|t| serde_json::to_string(t).ok())
        .collect();

    if lines.is_empty() {
        return;
    }

    let jsonl = lines.join("\n");
    let filename = format!("fabro-events-{}.jsonl", Uuid::new_v4());
    spawn_fabro_subcommand("__send_analytics", &filename, jsonl.as_bytes());
}

/// Parse JSONL content into a Segment batch payload.
///
/// Each non-empty line is parsed as JSON, has `"type": "track"` injected,
/// and is collected into a `{"batch": [...]}` wrapper.
/// Returns `None` if no valid events are found.
fn build_segment_batch(content: &str) -> Option<serde_json::Value> {
    let mut batch = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(line) {
            Ok(mut map) => {
                map.insert("type".into(), "track".into());
                batch.push(serde_json::Value::Object(map));
            }
            Err(err) => {
                tracing::warn!(%err, "skipping malformed JSONL line");
            }
        }
    }

    if batch.is_empty() {
        return None;
    }

    Some(serde_json::json!({ "batch": batch }))
}

/// Sends track events to Segment synchronously. Used for mid-run flushes
/// on the background telemetry thread.
/// No-ops if `SEGMENT_WRITE_KEY` was not set at compile time or `tracks` is
/// empty.
pub fn upload_blocking(tracks: &[Track]) -> anyhow::Result<()> {
    if tracks.is_empty() {
        return Ok(());
    }

    let write_key = SEGMENT_WRITE_KEY
        .ok_or_else(|| anyhow::anyhow!("SEGMENT_WRITE_KEY not set at compile time"))?;

    let lines: Vec<String> = tracks
        .iter()
        .filter_map(|t| serde_json::to_string(t).ok())
        .collect();

    let content = lines.join("\n");
    let Some(payload) = build_segment_batch(&content) else {
        return Ok(());
    };

    let auth = STANDARD.encode(format!("{write_key}:"));

    let resp = BlockingClient::new()
        .post(format!("{SEGMENT_BASE_URL}/v1/batch"))
        .header("Authorization", format!("Basic {auth}"))
        .json(&payload)
        .send()?;

    if !resp.status().is_success() {
        anyhow::bail!("segment API returned status {}", resp.status());
    }

    Ok(())
}

/// Reads a JSONL file of serialized track events from `path` and sends them
/// to Segment as a batch.
/// Called by the `__send_analytics` subcommand.
/// No-ops if `SEGMENT_WRITE_KEY` was not set at compile time.
pub async fn upload(path: &Path) -> anyhow::Result<()> {
    let write_key = SEGMENT_WRITE_KEY
        .ok_or_else(|| anyhow::anyhow!("SEGMENT_WRITE_KEY not set at compile time"))?;

    let content = std::fs::read_to_string(path)?;
    let Some(payload) = build_segment_batch(&content) else {
        return Ok(());
    };

    let auth = STANDARD.encode(format!("{write_key}:"));

    let resp = reqwest::Client::new()
        .post(format!("{SEGMENT_BASE_URL}/v1/batch"))
        .header("Authorization", format!("Basic {auth}"))
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("segment API returned status {}", resp.status());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::runtime::Runtime;

    use super::*;
    use crate::event::User;

    // -- Step 1: build_segment_batch tests --

    #[test]
    fn build_segment_batch_empty_content() {
        assert!(build_segment_batch("").is_none());
    }

    #[test]
    fn build_segment_batch_single_event() {
        let line = r#"{"anonymousId":"abc","event":"Test","properties":{},"messageId":"m1"}"#;
        let result = build_segment_batch(line).unwrap();

        let batch = result["batch"].as_array().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["type"], "track");
        assert_eq!(batch[0]["event"], "Test");
        assert_eq!(batch[0]["anonymousId"], "abc");
    }

    #[test]
    fn build_segment_batch_multiple_events() {
        let content = concat!(
            r#"{"anonymousId":"a","event":"E1","properties":{},"messageId":"m1"}"#,
            "\n",
            r#"{"anonymousId":"b","event":"E2","properties":{},"messageId":"m2"}"#,
        );
        let result = build_segment_batch(content).unwrap();

        let batch = result["batch"].as_array().unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0]["type"], "track");
        assert_eq!(batch[0]["event"], "E1");
        assert_eq!(batch[1]["type"], "track");
        assert_eq!(batch[1]["event"], "E2");
    }

    #[test]
    fn build_segment_batch_skips_malformed_lines() {
        let content = concat!(
            r#"{"anonymousId":"a","event":"Good","properties":{},"messageId":"m1"}"#,
            "\n",
            "this is not json",
        );
        let result = build_segment_batch(content).unwrap();

        let batch = result["batch"].as_array().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["event"], "Good");
    }

    #[test]
    fn build_segment_batch_all_malformed() {
        let content = "not json\nalso not json\n";
        assert!(build_segment_batch(content).is_none());
    }

    #[test]
    fn build_segment_batch_skips_blank_lines() {
        let content = concat!(
            "\n",
            r#"{"anonymousId":"a","event":"E1","properties":{},"messageId":"m1"}"#,
            "\n",
            "\n",
        );
        let result = build_segment_batch(content).unwrap();

        let batch = result["batch"].as_array().unwrap();
        assert_eq!(batch.len(), 1);
    }

    // -- Step 2: emit() tests --

    #[test]
    fn emit_noops_without_write_key() {
        // SEGMENT_WRITE_KEY is not set at compile time in tests,
        // so emit() should return immediately without spawning.
        let track = Track {
            user:       User::AnonymousId {
                anonymous_id: "test".to_string(),
            },
            event:      "test".to_string(),
            properties: json!({}),
            context:    None,
            timestamp:  None,
            message_id: "msg-test".to_string(),
        };

        // This should not panic or require a tokio runtime
        // because it returns before reaching spawn
        emit(&[track]);
    }

    #[test]
    fn emit_noops_with_empty_tracks() {
        emit(&[]);
    }

    // -- Step 3: upload_blocking() tests --

    #[test]
    fn upload_blocking_noops_without_write_key() {
        let track = Track {
            user:       User::AnonymousId {
                anonymous_id: "test".to_string(),
            },
            event:      "test".to_string(),
            properties: json!({}),
            context:    None,
            timestamp:  None,
            message_id: "msg-test".to_string(),
        };

        let result = upload_blocking(&[track]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("SEGMENT_WRITE_KEY not set"));
    }

    #[test]
    fn upload_blocking_noops_with_empty_tracks() {
        // Empty tracks returns Ok without checking credentials
        let result = upload_blocking(&[]);
        assert!(result.is_ok());
    }

    // -- Step 4: upload() tests --

    #[test]
    fn upload_noops_without_write_key() {
        // SEGMENT_WRITE_KEY is not set at compile time in tests, so this should error.
        let rt = Runtime::new().unwrap();
        let result = rt.block_on(upload(Path::new("/nonexistent")));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("SEGMENT_WRITE_KEY not set"));
    }
}
