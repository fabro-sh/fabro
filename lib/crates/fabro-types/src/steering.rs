use serde::{Deserialize, Serialize};

/// Two flavors of mid-run steering messages delivered to a live agent
/// session.
///
/// - `Append` — push to the steering queue; the agent picks it up at the next
///   turn boundary.
/// - `Interrupt` — cancel the in-flight LLM stream / tool call in the current
///   round, then deliver the message as the next user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SteerKind {
    Append,
    Interrupt,
}

impl SteerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Interrupt => "interrupt",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_round_trips_through_json() {
        let json = serde_json::to_string(&SteerKind::Append).unwrap();
        assert_eq!(json, "\"append\"");
        let parsed: SteerKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SteerKind::Append);
    }

    #[test]
    fn interrupt_round_trips_through_json() {
        let json = serde_json::to_string(&SteerKind::Interrupt).unwrap();
        assert_eq!(json, "\"interrupt\"");
        let parsed: SteerKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SteerKind::Interrupt);
    }

    #[test]
    fn unknown_value_fails_to_deserialize() {
        let result: Result<SteerKind, _> = serde_json::from_str("\"unknown\"");
        assert!(result.is_err());
    }
}
