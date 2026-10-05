//! GitHub credentials for every process in a run's execution scopes.
//!
//! Git reads a private, renewable store outside the workspace. Processes get
//! the helper configuration and, when requested, a fresh `GITHUB_TOKEN` at
//! spawn. A long-lived agent's Git commands read the renewed store. A
//! one-shot container gets the token alone: the store lives in the scope's
//! sandbox, which the container does not share.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fabro_github::token_source::InstallationTokenSource;
use fabro_github::{GitHubCredentials, GitHubRepositoryAccess};
use fabro_types::settings::run::RunMode;
use fabro_types::{GitHubRepositorySlug, RunSpec, RunTarget};
use fabro_util::shell;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use petri_runtime::executor::{
    AcquireContext, EnvError, EnvHandle, ExecEnv, Executor, Masker, ProcessSpec, ReleaseReport,
    ScopeOutcome, ScopeSpec, SpawnEnv, SpawnTarget,
};
use petri_runtime::ir::LogStream;
use smol_str::SmolStr;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::warn;

const REFRESH_INTERVAL: Duration = Duration::from_mins(1);
const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Token policy resolved by Fabro, without GitHub policy in Petri.
#[derive(Clone)]
pub struct StageCredentials {
    git_tokens:   Arc<InstallationTokenSource>,
    api_tokens:   Option<Arc<InstallationTokenSource>>,
    repositories: Vec<GitHubRepositorySlug>,
}

impl StageCredentials {
    /// Default origin access is read-only. Declared permissions govern the
    /// integration token and additional repositories. Static tokens retain
    /// their existing scope; publication has a separate write-token source.
    pub fn for_run(
        spec: &RunSpec,
        credentials: Option<&GitHubCredentials>,
    ) -> anyhow::Result<Option<Self>> {
        if spec.settings.run.execution.mode == RunMode::DryRun {
            return Ok(None);
        }
        let Some(RunTarget::Git(target)) = &spec.target else {
            return Ok(None);
        };
        let repository = target.clone().validate()?.repository().clone();
        let integration = spec
            .settings
            .run
            .integrations
            .github
            .resolve_integration()?;
        let access = GitHubRepositoryAccess::new(
            Some(&repository.https_url()),
            &integration.additional_repositories,
            integration.permissions.clone(),
        )?;
        let Some(credentials) = credentials else {
            return Ok(None);
        };
        let api_tokens = if integration.is_token_requested() {
            access
                .as_ref()
                .map(|access| InstallationTokenSource::for_access(credentials, access))
                .transpose()?
        } else {
            None
        };
        let (git_tokens, repositories) = if matches!(
            integration.permissions.get("contents").map(String::as_str),
            Some("read" | "write")
        ) {
            let Some(access) = access else {
                return Ok(None);
            };
            let Some(tokens) = &api_tokens else {
                return Ok(None);
            };
            (
                tokens.clone(),
                access.targets().into_iter().cloned().collect(),
            )
        } else {
            (
                InstallationTokenSource::for_repository(
                    credentials,
                    repository.owner().to_string(),
                    repository.repo().to_string(),
                    serde_json::json!({"contents":"read"}),
                )?,
                vec![repository],
            )
        };
        Ok(Some(Self {
            git_tokens,
            api_tokens,
            repositories,
        }))
    }

    pub(crate) fn executor(&self, inner: Arc<dyn Executor>, masker: Masker) -> Arc<dyn Executor> {
        Arc::new(CredentialExecutor {
            inner,
            credentials: self.clone(),
            masker,
            scopes: Mutex::new(HashMap::new()),
        })
    }
}

struct CredentialExecutor {
    inner:       Arc<dyn Executor>,
    credentials: StageCredentials,
    masker:      Masker,
    scopes:      Mutex<HashMap<String, ScopeRefresh>>,
}

struct ScopeRefresh {
    env:  Arc<CredentialEnv>,
    task: JoinHandle<()>,
}

impl Drop for ScopeRefresh {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[async_trait]
impl Executor for CredentialExecutor {
    async fn acquire(
        &self,
        scope: &ScopeSpec,
        ctx: &AcquireContext,
    ) -> Result<EnvHandle, EnvError> {
        let handle = self.inner.acquire(scope, ctx).await?;
        let inner = handle.exec();
        let directory = match command(
            &inner,
            "umask 077; mktemp -d /tmp/fabro-git-credentials.XXXXXXXX",
            Vec::new(),
        )
        .await
        {
            Ok(directory) => directory.trim().to_string(),
            Err(error) => {
                self.inner.release(handle, ScopeOutcome::Failed).await;
                return Err(error);
            }
        };
        let env = Arc::new(CredentialEnv {
            inner,
            credentials: self.credentials.clone(),
            masker: self.masker.clone(),
            directory,
            update: Mutex::new(()),
        });
        if let Err(error) = env.refresh().await {
            let _ = env.cleanup().await;
            self.inner.release(handle, ScopeOutcome::Failed).await;
            return Err(error);
        }
        let refresh_env = env.clone();
        let task = tokio::spawn(async move {
            loop {
                time::sleep(REFRESH_INTERVAL).await;
                if let Err(error) = refresh_env.refresh().await {
                    warn!(error = %refresh_env.masker.mask(&error.to_string()), "could not refresh the sandbox's GitHub credentials; retrying");
                }
            }
        });
        self.scopes
            .lock()
            .await
            .insert(handle.instance().to_string(), ScopeRefresh {
                env: env.clone(),
                task,
            });
        Ok(handle.with_spawn_env(env))
    }

    async fn release(&self, handle: EnvHandle, outcome: ScopeOutcome) -> ReleaseReport {
        let mut problems = Vec::new();
        let refresh = self.scopes.lock().await.remove(handle.instance());
        if let Some(mut refresh) = refresh {
            refresh.task.abort();
            let _ = (&mut refresh.task).await;
            if refresh.env.cleanup().await.is_err() {
                problems.push("could not remove the sandbox's GitHub credential store".to_string());
            }
        }
        let mut report = self.inner.release(handle, outcome).await;
        report.problems.extend(problems);
        report
    }
}

struct CredentialEnv {
    inner:       Arc<dyn ExecEnv>,
    credentials: StageCredentials,
    masker:      Masker,
    directory:   String,
    update:      Mutex<()>,
}

impl CredentialEnv {
    async fn refresh(&self) -> Result<(), EnvError> {
        let _update = self.update.lock().await;
        let token = time::timeout(CREDENTIAL_TIMEOUT, self.credentials.git_tokens.resolve())
            .await
            .map_err(|_| EnvError::backend("github", "refresh", "token resolution timed out"))?
            .map_err(|_| EnvError::backend("github", "refresh", "token resolution failed"))?;
        self.masker.register_explicit(token.token.expose());
        let password = utf8_percent_encode(token.token.expose(), NON_ALPHANUMERIC).to_string();
        self.masker.register_explicit(&password);
        let store = self
            .credentials
            .repositories
            .iter()
            .flat_map(|repo| {
                [
                    format!("https://x-access-token:{password}@github.com/{repo}\n"),
                    format!("https://x-access-token:{password}@github.com/{repo}.git\n"),
                ]
            })
            .collect::<String>();
        self.masker.register_explicit(&store);
        let path = shell::shell_quote(&format!("{}/store", self.directory));
        command(&self.inner, &format!("umask 077; printf '%s' \"$FABRO_GIT_CREDENTIAL_STORE\" > {path}.new && mv -f {path}.new {path}"), vec![("FABRO_GIT_CREDENTIAL_STORE".to_string(), store)]).await?;
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), EnvError> {
        command(
            &self.inner,
            &format!("rm -rf -- {}", shell::shell_quote(&self.directory)),
            Vec::new(),
        )
        .await
        .map(|_| ())
    }

    fn git_env(&self, env: &mut BTreeMap<SmolStr, SmolStr>) -> Result<(), EnvError> {
        let count = env
            .get("GIT_CONFIG_COUNT")
            .map(ToString::to_string)
            .or_else(|| self.inner.ambient_env("GIT_CONFIG_COUNT"))
            .unwrap_or_else(|| "0".to_string())
            .parse::<usize>()
            .ok()
            .filter(|count| *count <= 256)
            .ok_or_else(|| {
                EnvError::backend("github", "environment", "invalid GIT_CONFIG_COUNT")
            })?;
        // Preserve fetch/publish headers and workflow configuration. The store
        // returns a credential only for an allowed repository path.
        let entries = [
            (
                "credential.https://github.com.useHttpPath",
                "true".to_string(),
            ),
            ("credential.https://github.com.helper", String::new()),
            (
                "credential.https://github.com.helper",
                format!(
                    "store --file={}",
                    shell::shell_quote(&format!("{}/store", self.directory))
                ),
            ),
            (
                "url.https://github.com/.insteadOf",
                "git@github.com:".to_string(),
            ),
            (
                "url.https://github.com/.insteadOf",
                "ssh://git@github.com/".to_string(),
            ),
        ];
        for index in 0..count {
            for prefix in ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"] {
                let key = format!("{prefix}{index}");
                if !env.contains_key(key.as_str()) {
                    if let Some(value) = self.inner.ambient_env(&key) {
                        env.insert(key.into(), value.into());
                    }
                }
            }
        }
        for (offset, (key, value)) in entries.iter().enumerate() {
            env.insert(
                format!("GIT_CONFIG_KEY_{}", count + offset).into(),
                (*key).into(),
            );
            env.insert(
                format!("GIT_CONFIG_VALUE_{}", count + offset).into(),
                value.as_str().into(),
            );
        }
        env.insert(
            "GIT_CONFIG_COUNT".into(),
            (count + entries.len()).to_string().into(),
        );
        env.entry("GIT_TERMINAL_PROMPT".into())
            .or_insert_with(|| "0".into());
        Ok(())
    }
}

#[async_trait]
impl SpawnEnv for CredentialEnv {
    async fn apply(
        &self,
        target: SpawnTarget,
        env: &mut BTreeMap<SmolStr, SmolStr>,
    ) -> Result<(), EnvError> {
        // The store and the helper configuration that names it live in the
        // scope's sandbox; a one-shot container cannot read either.
        if target == SpawnTarget::Process {
            self.refresh().await?;
        }
        // The managed token carries exactly the access the run declared, so
        // it replaces any `GITHUB_TOKEN` the process would otherwise see, as
        // Fabro's stage environment did before Petri.
        if let Some(tokens) = &self.credentials.api_tokens {
            let resolved = time::timeout(CREDENTIAL_TIMEOUT, tokens.resolve())
                .await
                .map_err(|_| EnvError::backend("github", "resolve", "token resolution timed out"))?
                .map_err(|_| EnvError::backend("github", "resolve", "token resolution failed"))?;
            self.masker.register_explicit(resolved.token.expose());
            env.insert("GITHUB_TOKEN".into(), resolved.token.expose().into());
        }
        if target == SpawnTarget::Process {
            self.git_env(env)?;
        }
        Ok(())
    }
}

async fn command(
    env: &Arc<dyn ExecEnv>,
    script: &str,
    extra_env: Vec<(String, String)>,
) -> Result<String, EnvError> {
    let spec = ProcessSpec::new("sh", &["-c", script])
        .with_timeout(Some(CREDENTIAL_TIMEOUT))
        .with_env(
            extra_env
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        );
    let mut handle = env.spawn(spec).await?;
    let mut stdout = String::new();
    if let Some(mut lines) = handle.lines() {
        while let Some(line) = lines.recv().await {
            // Never echo stderr from an operation installing credentials.
            if line.stream == LogStream::Stdout {
                stdout.push_str(&line.line);
                stdout.push('\n');
            }
        }
    }
    if !handle.wait().await?.is_success() {
        return Err(EnvError::backend(
            "github",
            "credential_store",
            "credential store operation failed",
        ));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use fabro_github::InstallationToken;
    use fabro_github::test_support::{self, InstallationTokenMinter};
    use fabro_types::test_support as types_support;
    use petri_runtime::executor::{MapSecrets, SecretProvider as _, StdinMode};
    use petri_runtime::ir::ScopeId;
    use tokio::io::AsyncWriteExt as _;

    use super::*;
    use crate::providers::{self, SandboxProviderConfig};

    struct RotatingMinter(AtomicUsize);

    #[async_trait]
    impl InstallationTokenMinter for RotatingMinter {
        async fn mint(&self) -> anyhow::Result<InstallationToken> {
            let generation = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(InstallationToken {
                token:      format!("scripted-token-generation-{generation}"),
                // Each resolve exercises renewal without waiting an hour.
                expires_at: Utc::now() + chrono::Duration::minutes(5),
            })
        }
    }

    fn credentials(api: bool) -> StageCredentials {
        let tokens = test_support::installation_token_source(
            "acme/private",
            Arc::new(RotatingMinter(AtomicUsize::new(0))),
        );
        StageCredentials {
            git_tokens:   tokens.clone(),
            api_tokens:   api.then_some(tokens),
            repositories: vec![GitHubRepositorySlug::try_new("acme/private").expect("slug")],
        }
    }

    #[test]
    fn default_origin_access_does_not_request_an_api_token() {
        let mut spec = types_support::test_run_spec();
        spec.target = Some(
            serde_json::from_value(
                serde_json::json!({"kind":"git", "repo":"acme/private", "branch":"main"}),
            )
            .expect("target"),
        );
        let pat = GitHubCredentials::Pat("scripted-personal-access-token".to_string());
        let default = StageCredentials::for_run(&spec, Some(&pat))
            .expect("policy")
            .expect("credentials");
        assert!(default.api_tokens.is_none());
        assert_eq!(default.repositories.len(), 1);
        spec.settings
            .run
            .integrations
            .github
            .permissions
            .insert("contents".to_string(), "read".into());
        spec.settings
            .run
            .integrations
            .github
            .additional_repositories
            .insert(GitHubRepositorySlug::try_new("acme/another").expect("slug"));
        let declared = StageCredentials::for_run(&spec, Some(&pat))
            .expect("policy")
            .expect("credentials");
        assert!(declared.api_tokens.is_some());
        assert_eq!(declared.repositories.len(), 2);
    }

    #[tokio::test]
    async fn a_running_process_reads_renewed_git_credentials_and_release_cleans_up() {
        let dir = tempfile::tempdir().expect("directory");
        let runtime = providers::standard_runtime(&SandboxProviderConfig::default());
        let router = runtime.sandbox_router_for(dir.path()).expect("router");
        let secrets = MapSecrets::empty();
        let executor = credentials(false).executor(router, secrets.masker());
        let handle = executor
            .acquire(
                &ScopeSpec::new(ScopeId::new(0), "credentials"),
                &AcquireContext::bare(),
            )
            .await
            .expect("acquire");
        let env = handle.exec();
        let script = "printf 'protocol=https\\nhost=github.com\\npath=acme/private\\n\\n' | git credential fill; printf 'READY\\n'; read answer; printf 'protocol=https\\nhost=github.com\\npath=acme/private\\n\\n' | git credential fill";
        let mut process = env
            .spawn(ProcessSpec::new("sh", &["-c", script]).with_stdin(StdinMode::Piped))
            .await
            .expect("launch");
        let mut lines = process.lines().expect("lines");
        let mut first = String::new();
        while let Some(line) = lines.recv().await {
            if line.line == "READY" {
                break;
            }
            first.push_str(&line.line);
        }
        assert!(
            first.contains("password=scripted-token-generation-2"),
            "first token is delivered (password present: {}, masked: {}, stderr present: {})",
            first.contains("password="),
            first.contains("***"),
            first.contains("fatal:")
        );
        // Another spawn rotates the store while the original process lives.
        command(&env, "true", Vec::new())
            .await
            .expect("refresh via spawn");
        process
            .stdin()
            .expect("stdin")
            .write_all(b"continue\n")
            .await
            .expect("continue");
        let mut second = String::new();
        while let Some(line) = lines.recv().await {
            second.push_str(&line.line);
        }
        assert!(
            second.contains("password=scripted-token-generation-3"),
            "the same process uses the renewed helper"
        );
        assert!(process.wait().await.expect("exit").is_success());
        assert!(
            !secrets
                .masker()
                .mask(&format!("{first}{second}"))
                .contains("scripted-token")
        );
        assert!(command(&env, "printf 'protocol=https\\nhost=github.com\\npath=acme/unrelated\\n\\n' | git credential fill", Vec::new()).await.is_err(), "the helper refuses an undeclared repository");
        command(&env, "printf 'protocol=https\\nhost=github.com\\npath=acme/private.git\\n\\n' | git credential fill >/dev/null", Vec::new()).await.expect("the .git spelling is authenticated too");
        let probe = command(
            &env,
            "git config --get credential.https://github.com.helper",
            Vec::new(),
        )
        .await
        .expect("helper");
        let file = probe
            .trim()
            .strip_prefix("store --file=")
            .expect("store path");
        assert_eq!(
            fs::metadata(file).expect("file").permissions().mode() & 0o777,
            0o600
        );
        let report = executor.release(handle, ScopeOutcome::Succeeded).await;
        assert!(report.is_clean(), "{report:?}");
        assert!(
            !Path::new(file).exists(),
            "release removes the credential store"
        );
    }

    #[tokio::test]
    async fn a_container_gets_the_managed_token_but_no_store_configuration() {
        let dir = tempfile::tempdir().expect("directory");
        let runtime = providers::standard_runtime(&SandboxProviderConfig::default());
        let router = runtime.sandbox_router_for(dir.path()).expect("router");
        let handle = router
            .acquire(
                &ScopeSpec::new(ScopeId::new(0), "container-credentials"),
                &AcquireContext::bare(),
            )
            .await
            .expect("acquire");
        let store = dir.path().join("store-directory");
        let secrets = MapSecrets::empty();
        let env = CredentialEnv {
            inner:       handle.exec(),
            credentials: credentials(true),
            masker:      secrets.masker(),
            directory:   store.display().to_string(),
            update:      Mutex::new(()),
        };
        let mut container = BTreeMap::from([("GITHUB_TOKEN".into(), "explicit".into())]);
        env.apply(SpawnTarget::Container, &mut container)
            .await
            .expect("apply");
        assert!(
            container["GITHUB_TOKEN"].starts_with("scripted-token-generation-"),
            "the managed token replaces an explicit one in a container too"
        );
        assert!(
            !container.keys().any(|key| key.starts_with("GIT_CONFIG")),
            "no helper configuration names the scope's store"
        );
        assert!(!store.exists(), "a container spawn writes no store");
        let mut process = BTreeMap::new();
        fs::create_dir(&store).expect("store directory");
        env.apply(SpawnTarget::Process, &mut process)
            .await
            .expect("apply");
        assert!(process.contains_key("GIT_CONFIG_COUNT"));
        assert!(
            store.join("store").exists(),
            "a process spawn refreshes the store"
        );
        assert!(
            router
                .release(handle, ScopeOutcome::Succeeded)
                .await
                .is_clean()
        );
    }

    #[tokio::test]
    async fn declared_api_tokens_reach_processes_and_replace_explicit_values() {
        let dir = tempfile::tempdir().expect("directory");
        let runtime = providers::standard_runtime(&SandboxProviderConfig::default());
        let router = runtime.sandbox_router_for(dir.path()).expect("router");
        let secrets = MapSecrets::empty();
        let executor = credentials(true).executor(router, secrets.masker());
        let handle = executor
            .acquire(
                &ScopeSpec::new(ScopeId::new(0), "api-credentials"),
                &AcquireContext::bare(),
            )
            .await
            .expect("acquire");
        let env = handle.exec();
        command(
            &env,
            "case $GITHUB_TOKEN in scripted-token-generation-*) exit 0;; *) exit 1;; esac",
            Vec::new(),
        )
        .await
        .expect("the integration token reaches the child");
        command(
            &env,
            "case $GITHUB_TOKEN in scripted-token-generation-*) exit 0;; *) exit 1;; esac",
            vec![(
                "GITHUB_TOKEN".to_string(),
                "command-token-override".to_string(),
            )],
        )
        .await
        .expect("the managed token replaces an explicit one");
        assert!(
            secrets
                .masker()
                .contains_secret("scripted-token-generation-3")
        );
        assert!(
            executor
                .release(handle, ScopeOutcome::Succeeded)
                .await
                .is_clean()
        );
    }
}
