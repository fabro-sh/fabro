//! Which workspace a Petri scope runs in, from the run's own records.
//!
//! Petri names an isolated scope's workspace after its invocation and scope
//! (`invocation-<n>-scope-<m>`), and a nested invocation that inherits its
//! caller's sandbox shares the caller's workspace through the lease the
//! coordinator recorded. The hooks and recovery both need the workspace id
//! behind a scope, and both read it the same way here: the direct name
//! when its workspace exists on this host, else the lease the invocation's
//! declaration names, resolved through the resource log.

use std::sync::Arc;

use petri_execution::host::{self, HostError};
use petri_execution::{
    Access, InvocationId, ResourceError, ResourceStore, RunKey, RunLogs, RunStore, SandboxBinding,
};
use petri_runtime::executor::WorkspaceId;
use petri_runtime::ir::ScopeId;
use petri_store::StoreError;

/// Why a workspace could not be named from the run's records.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceLookupError {
    #[error("the run's record could not be opened")]
    Open(#[source] StoreError),
    #[error("the run's coordinator state could not be read")]
    State(#[source] HostError),
    #[error("the run's resource log could not be read")]
    Resources(#[source] ResourceError),
    #[error("invocation {invocation} is not in the run's record")]
    UnknownInvocation { invocation: InvocationId },
}

/// The workspace id of an isolated scope: what the coordinator allocates
/// for `scope` in `invocation`.
#[must_use]
pub fn isolated_workspace(invocation: InvocationId, scope: ScopeId) -> String {
    WorkspaceId::scoped(Some(&invocation.workspace_prefix()), scope)
        .as_str()
        .to_owned()
}

/// The workspaces of a run, read from its records through a handle that
/// holds no lease.
pub struct WorkspaceLookup {
    store: Arc<dyn RunStore>,
    key:   RunKey,
}

impl WorkspaceLookup {
    #[must_use]
    pub fn new(store: Arc<dyn RunStore>, key: RunKey) -> Self {
        Self { store, key }
    }

    async fn logs(&self) -> Result<Arc<dyn RunLogs>, WorkspaceLookupError> {
        self.store
            .open(&self.key, Access::Read)
            .await
            .map_err(WorkspaceLookupError::Open)
    }

    /// The workspace an invocation inherited from its caller, or `None`
    /// when the invocation owns its sandboxes.
    pub async fn inherited(
        &self,
        invocation: InvocationId,
    ) -> Result<Option<String>, WorkspaceLookupError> {
        let logs = self.logs().await?;
        let state = host::stored_state(&*logs)
            .await
            .map_err(WorkspaceLookupError::State)?;
        let declared = state
            .invocations
            .get(&invocation)
            .ok_or(WorkspaceLookupError::UnknownInvocation { invocation })?;
        match declared.declaration.sandbox {
            SandboxBinding::Isolated => Ok(None),
            SandboxBinding::Inherited { lease } => {
                let resources = ResourceStore::load(&logs)
                    .await
                    .map_err(WorkspaceLookupError::Resources)?;
                let record = resources
                    .resolve(lease)
                    .map_err(WorkspaceLookupError::Resources)?;
                Ok(Some(record.workspace.as_str().to_owned()))
            }
        }
    }

    /// Every workspace an invocation runs in: its own leases' workspaces,
    /// or the one it inherited.
    pub async fn of_invocation(
        &self,
        invocation: InvocationId,
    ) -> Result<Vec<String>, WorkspaceLookupError> {
        if let Some(inherited) = self.inherited(invocation).await? {
            return Ok(vec![inherited]);
        }
        let logs = self.logs().await?;
        let resources = ResourceStore::load(&logs)
            .await
            .map_err(WorkspaceLookupError::Resources)?;
        let mut workspaces: Vec<String> = resources
            .records()
            .filter(|record| {
                record.allocation.invocation == invocation
                    && record.state != petri_execution::LeaseState::Deleted
            })
            .map(|record| record.workspace.as_str().to_owned())
            .collect();
        workspaces.sort();
        workspaces.dedup();
        Ok(workspaces)
    }
}
