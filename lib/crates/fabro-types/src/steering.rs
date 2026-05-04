use serde::{Deserialize, Serialize};

/// Whether a steer appends to the queue or interrupts the current round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SteerKind {
    Append,
    Interrupt,
}
