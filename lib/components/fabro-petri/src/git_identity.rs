//! The checkpoint identity on every process a run's scopes start.
//!
//! This is independent of GitHub credentials and repository configuration:
//! commands, agent tools and additional repositories all get the same identity.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_checkpoint::author::GitAuthor;
use petri_runtime::executor::{
    AcquireContext, EnvError, EnvHandle, Executor, ReleaseReport, ScopeOutcome, ScopeSpec,
    SpawnEnv, SpawnTarget,
};
use smol_str::SmolStr;

pub(crate) fn executor(inner: Arc<dyn Executor>, author: GitAuthor) -> Arc<dyn Executor> {
    Arc::new(IdentityExecutor {
        inner,
        identity: Arc::new(IdentityEnv(author)),
    })
}

struct IdentityExecutor {
    inner:    Arc<dyn Executor>,
    identity: Arc<IdentityEnv>,
}

#[async_trait]
impl Executor for IdentityExecutor {
    async fn acquire(
        &self,
        scope: &ScopeSpec,
        ctx: &AcquireContext,
    ) -> Result<EnvHandle, EnvError> {
        let handle = self.inner.acquire(scope, ctx).await?;
        Ok(handle.with_spawn_env(self.identity.clone()))
    }

    async fn release(&self, handle: EnvHandle, outcome: ScopeOutcome) -> ReleaseReport {
        self.inner.release(handle, outcome).await
    }
}

struct IdentityEnv(GitAuthor);

#[async_trait]
impl SpawnEnv for IdentityEnv {
    async fn apply(
        &self,
        _target: SpawnTarget,
        env: &mut BTreeMap<SmolStr, SmolStr>,
    ) -> Result<(), EnvError> {
        // The run selects one identity for workflow and checkpoint commits,
        // replacing conflicting stage or ambient values without writing Git config.
        for (key, value) in [
            ("GIT_AUTHOR_NAME", &self.0.name),
            ("GIT_AUTHOR_EMAIL", &self.0.email),
            ("GIT_COMMITTER_NAME", &self.0.name),
            ("GIT_COMMITTER_EMAIL", &self.0.email),
        ] {
            env.insert(key.into(), value.as_str().into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identity_reaches_processes_and_containers_and_replaces_conflicts() {
        let identity = IdentityEnv(GitAuthor {
            name:  "Run Author".into(),
            email: "run@example.com".into(),
        });
        for target in [SpawnTarget::Process, SpawnTarget::Container] {
            let mut env = BTreeMap::from([
                ("GIT_AUTHOR_NAME".into(), "Stage Author".into()),
                ("GIT_AUTHOR_EMAIL".into(), "stage@example.com".into()),
                ("GIT_COMMITTER_NAME".into(), "Stage Committer".into()),
                ("GIT_COMMITTER_EMAIL".into(), "committer@example.com".into()),
                ("OTHER".into(), "preserved".into()),
            ]);
            identity
                .apply(target, &mut env)
                .await
                .expect("identity applies");
            assert_eq!(
                env,
                BTreeMap::from([
                    ("GIT_AUTHOR_NAME".into(), "Run Author".into()),
                    ("GIT_AUTHOR_EMAIL".into(), "run@example.com".into()),
                    ("GIT_COMMITTER_NAME".into(), "Run Author".into()),
                    ("GIT_COMMITTER_EMAIL".into(), "run@example.com".into()),
                    ("OTHER".into(), "preserved".into()),
                ])
            );
        }
    }
}
