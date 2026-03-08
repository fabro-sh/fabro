pub mod anonymous_id;
pub mod context;
pub mod event;
pub mod sender;

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use event::{Track, User};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryLevel {
    Off,
    Errors,
    All,
}

pub struct Telemetry {
    level: TelemetryLevel,
    anonymous_id: String,
    context: Value,
}

impl Telemetry {
    pub fn for_server() -> Result<Self> {
        let level = telemetry_level();
        let anonymous_id = anonymous_id::load_or_create_server_id()?;
        let context = context::build_context();

        Ok(Self {
            level,
            anonymous_id,
            context,
        })
    }

    pub fn for_cli() -> Result<Self> {
        let level = telemetry_level();
        let anonymous_id = anonymous_id::compute_cli_id()?;
        let context = context::build_context();

        Ok(Self {
            level,
            anonymous_id,
            context,
        })
    }

    pub fn level(&self) -> &TelemetryLevel {
        &self.level
    }

    pub fn should_track(&self, is_error: bool) -> bool {
        match self.level {
            TelemetryLevel::Off => false,
            TelemetryLevel::Errors => is_error,
            TelemetryLevel::All => true,
        }
    }

    pub fn build_track(&self, event: &str, properties: Value) -> Track {
        Track {
            user: User::AnonymousId {
                anonymous_id: self.anonymous_id.clone(),
            },
            event: event.to_string(),
            properties,
            context: Some(self.context.clone()),
            timestamp: Some(Utc::now().to_rfc3339()),
            message_id: Uuid::new_v4().to_string(),
        }
    }
}

pub fn telemetry_level() -> TelemetryLevel {
    match std::env::var("ARC_TELEMETRY").as_deref() {
        Ok("off") => TelemetryLevel::Off,
        Ok("errors") => TelemetryLevel::Errors,
        Ok("all") => TelemetryLevel::All,
        _ => {
            if cfg!(debug_assertions) {
                TelemetryLevel::Off
            } else {
                TelemetryLevel::All
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn telemetry_level_defaults_to_off_in_debug() {
        // In test builds (debug_assertions=true), default is Off
        std::env::remove_var("ARC_TELEMETRY");
        assert_eq!(telemetry_level(), TelemetryLevel::Off);
    }

    #[test]
    fn telemetry_level_parses_env_var() {
        std::env::set_var("ARC_TELEMETRY", "all");
        assert_eq!(telemetry_level(), TelemetryLevel::All);

        std::env::set_var("ARC_TELEMETRY", "errors");
        assert_eq!(telemetry_level(), TelemetryLevel::Errors);

        std::env::set_var("ARC_TELEMETRY", "off");
        assert_eq!(telemetry_level(), TelemetryLevel::Off);

        // Clean up
        std::env::remove_var("ARC_TELEMETRY");
    }

    #[test]
    fn should_track_respects_level() {
        let telemetry = Telemetry {
            level: TelemetryLevel::Off,
            anonymous_id: "test".to_string(),
            context: json!({}),
        };
        assert!(!telemetry.should_track(false));
        assert!(!telemetry.should_track(true));

        let telemetry = Telemetry {
            level: TelemetryLevel::Errors,
            anonymous_id: "test".to_string(),
            context: json!({}),
        };
        assert!(!telemetry.should_track(false));
        assert!(telemetry.should_track(true));

        let telemetry = Telemetry {
            level: TelemetryLevel::All,
            anonymous_id: "test".to_string(),
            context: json!({}),
        };
        assert!(telemetry.should_track(false));
        assert!(telemetry.should_track(true));
    }

    #[test]
    fn build_track_populates_fields() {
        let telemetry = Telemetry {
            level: TelemetryLevel::All,
            anonymous_id: "anon-123".to_string(),
            context: json!({"app": {"name": "arc"}}),
        };

        let track = telemetry.build_track("Test Event", json!({"key": "value"}));
        assert_eq!(track.event, "Test Event");
        assert_eq!(track.properties["key"], "value");
        assert!(track.context.is_some());
        assert!(track.timestamp.is_some());
        assert!(!track.message_id.is_empty());

        match &track.user {
            User::AnonymousId { anonymous_id } => assert_eq!(anonymous_id, "anon-123"),
            _ => panic!("expected AnonymousId variant"),
        }
    }
}
