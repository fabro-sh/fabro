use serde::{Deserialize, Serialize};

use crate::{ExecOutputTail, FailureCategory, FailureReason, FailureSignature, SystemActorKind};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFailure {
    pub message:          String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes:           Vec<String>,
    pub reason:           FailureReason,
    pub category:         FailureCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_actor:     Option<SystemActorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature:        Option<FailureSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_output_tail: Option<ExecOutputTail>,
}
