use serde::{Deserialize, Serialize};

use crate::{RunSandboxInstance, SandboxStatus};

/// The sandbox owned by a run: fabro's record of it, and the status the
/// sandbox driver reports for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxDetails {
    pub sandbox: RunSandboxInstance,
    pub status:  SandboxStatus,
}
