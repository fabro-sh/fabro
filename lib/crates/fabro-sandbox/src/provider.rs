#[cfg(feature = "daytona")]
pub mod daytona;
#[cfg(feature = "docker")]
pub mod docker;

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(any(feature = "docker", feature = "daytona"))]
use fabro_github::GitHubCredentials;
#[cfg(any(feature = "docker", feature = "daytona"))]
use fabro_types::RunId;
use fabro_types::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxProviderKind,
    SandboxProviderLookupError,
};

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaConfig;
#[cfg(feature = "docker")]
use crate::docker::DockerSandboxOptions;

#[derive(Debug, Clone, Default)]
pub struct SandboxListFilter;

pub enum SandboxCreateSpec {
    Local,
    #[cfg(feature = "docker")]
    Docker {
        config:           DockerSandboxOptions,
        github_app:       Option<GitHubCredentials>,
        run_id:           Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch:     Option<String>,
    },
    #[cfg(feature = "daytona")]
    Daytona {
        config:           Box<DaytonaConfig>,
        github_app:       Option<GitHubCredentials>,
        run_id:           Option<RunId>,
        clone_origin_url: Option<String>,
        clone_branch:     Option<String>,
        api_key:          Option<String>,
    },
}

#[async_trait]
pub trait SandboxProvider: Send + Sync {
    fn kind(&self) -> SandboxProviderKind;

    async fn list(&self, filter: SandboxListFilter) -> crate::Result<Vec<SandboxInfo>>;
    async fn get(&self, id: &str) -> crate::Result<Option<SandboxInfo>>;
    async fn create(&self, spec: SandboxCreateSpec) -> crate::Result<SandboxInfo>;
    async fn delete(&self, id: &str) -> crate::Result<()>;
}

#[derive(Clone, Default)]
pub struct SandboxProviderRegistry {
    providers: Vec<Arc<dyn SandboxProvider>>,
}

impl SandboxProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn SandboxProvider>>) -> Self {
        Self { providers }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn providers(&self) -> &[Arc<dyn SandboxProvider>] {
        &self.providers
    }

    pub async fn list_managed(&self) -> SandboxListResponse {
        let mut data = Vec::new();
        let mut provider_errors = Vec::new();

        for provider in &self.providers {
            match provider.list(SandboxListFilter).await {
                Ok(mut sandboxes) => data.append(&mut sandboxes),
                Err(err) => provider_errors.push(provider_error(provider.kind(), &err)),
            }
        }

        SandboxListResponse {
            data,
            meta: SandboxListMeta { provider_errors },
        }
    }

    pub async fn get_managed_by_native_id(
        &self,
        id: &str,
    ) -> Result<SandboxInfo, SandboxLookupError> {
        let mut matches = Vec::new();
        let mut provider_errors = Vec::new();

        for provider in &self.providers {
            match provider.get(id).await {
                Ok(Some(sandbox)) => matches.push(sandbox),
                Ok(None) => {}
                Err(err) => provider_errors.push(provider_error(provider.kind(), &err)),
            }
        }

        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 if provider_errors.is_empty() => {
                Err(SandboxLookupError::NotFound { id: id.to_string() })
            }
            0 => Err(SandboxLookupError::ProviderUnavailable {
                id: id.to_string(),
                provider_errors,
            }),
            _ => Err(SandboxLookupError::Conflict {
                id:        id.to_string(),
                providers: matches
                    .into_iter()
                    .map(|sandbox| sandbox.provider)
                    .collect(),
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxLookupError {
    #[error("sandbox '{id}' was not found by any configured provider")]
    NotFound { id: String },
    #[error("sandbox '{id}' matched more than one configured provider")]
    Conflict {
        id:        String,
        providers: Vec<SandboxProviderKind>,
    },
    #[error("sandbox '{id}' could not be found definitively because one or more providers failed")]
    ProviderUnavailable {
        id:              String,
        provider_errors: Vec<SandboxProviderLookupError>,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalSandboxProvider;

#[async_trait]
impl SandboxProvider for LocalSandboxProvider {
    fn kind(&self) -> SandboxProviderKind {
        SandboxProviderKind::Local
    }

    async fn list(&self, _filter: SandboxListFilter) -> crate::Result<Vec<SandboxInfo>> {
        Ok(Vec::new())
    }

    async fn get(&self, _id: &str) -> crate::Result<Option<SandboxInfo>> {
        Ok(None)
    }

    async fn create(&self, _spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
        Err(crate::Error::message(
            "local sandbox provider has no provider-managed inventory",
        ))
    }

    async fn delete(&self, _id: &str) -> crate::Result<()> {
        Ok(())
    }
}

fn provider_error(
    provider: SandboxProviderKind,
    err: &(dyn std::error::Error + 'static),
) -> SandboxProviderLookupError {
    SandboxProviderLookupError {
        provider,
        message: render_error(err),
    }
}

fn render_error(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        let _ = write!(message, ": {err}");
        source = err.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fabro_types::{SandboxNetwork, SandboxResources, SandboxState, SandboxTimestamps};

    use super::*;

    #[derive(Clone)]
    enum FakeList {
        Ok(Vec<SandboxInfo>),
        Err(&'static str),
    }

    #[derive(Clone)]
    enum FakeGet {
        Found(Box<SandboxInfo>),
        Missing,
        Err(&'static str),
    }

    struct FakeProvider {
        kind: SandboxProviderKind,
        list: FakeList,
        get:  FakeGet,
    }

    impl FakeProvider {
        fn new(kind: SandboxProviderKind, list: FakeList, get: FakeGet) -> Self {
            Self { kind, list, get }
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeProvider {
        fn kind(&self) -> SandboxProviderKind {
            self.kind
        }

        async fn list(&self, _filter: SandboxListFilter) -> crate::Result<Vec<SandboxInfo>> {
            match &self.list {
                FakeList::Ok(sandboxes) => Ok(sandboxes.clone()),
                FakeList::Err(message) => Err(crate::Error::message(*message)),
            }
        }

        async fn get(&self, _id: &str) -> crate::Result<Option<SandboxInfo>> {
            match &self.get {
                FakeGet::Found(sandbox) => Ok(Some((**sandbox).clone())),
                FakeGet::Missing => Ok(None),
                FakeGet::Err(message) => Err(crate::Error::message(*message)),
            }
        }

        async fn create(&self, _spec: SandboxCreateSpec) -> crate::Result<SandboxInfo> {
            Err(crate::Error::message("not implemented"))
        }

        async fn delete(&self, _id: &str) -> crate::Result<()> {
            Ok(())
        }
    }

    fn registry(providers: Vec<FakeProvider>) -> SandboxProviderRegistry {
        SandboxProviderRegistry::new(
            providers
                .into_iter()
                .map(|provider| Arc::new(provider) as Arc<dyn SandboxProvider>)
                .collect(),
        )
    }

    fn info(provider: SandboxProviderKind, id: &str) -> SandboxInfo {
        SandboxInfo {
            provider,
            id: id.to_string(),
            display_name: None,
            state: SandboxState::Running,
            native_state: None,
            image: None,
            snapshot: None,
            region: None,
            web_url: None,
            working_directory: None,
            resources: SandboxResources::default(),
            network: SandboxNetwork::unknown(),
            labels: BTreeMap::new(),
            timestamps: SandboxTimestamps::default(),
        }
    }

    #[tokio::test]
    async fn list_returns_aggregate_data_from_successful_providers() {
        let docker = info(SandboxProviderKind::Docker, "docker-1");
        let daytona = info(SandboxProviderKind::Daytona, "daytona-1");
        let registry = registry(vec![
            FakeProvider::new(
                SandboxProviderKind::Docker,
                FakeList::Ok(vec![docker.clone()]),
                FakeGet::Missing,
            ),
            FakeProvider::new(
                SandboxProviderKind::Daytona,
                FakeList::Ok(vec![daytona.clone()]),
                FakeGet::Missing,
            ),
        ]);

        let response = registry.list_managed().await;

        assert_eq!(response.data, vec![docker, daytona]);
        assert!(response.meta.provider_errors.is_empty());
    }

    #[tokio::test]
    async fn list_includes_provider_error_metadata_when_one_provider_fails() {
        let docker = info(SandboxProviderKind::Docker, "docker-1");
        let registry = registry(vec![
            FakeProvider::new(
                SandboxProviderKind::Docker,
                FakeList::Ok(vec![docker.clone()]),
                FakeGet::Missing,
            ),
            FakeProvider::new(
                SandboxProviderKind::Daytona,
                FakeList::Err("daytona unavailable"),
                FakeGet::Missing,
            ),
        ]);

        let response = registry.list_managed().await;

        assert_eq!(response.data, vec![docker]);
        assert_eq!(response.meta.provider_errors, vec![
            SandboxProviderLookupError {
                provider: SandboxProviderKind::Daytona,
                message:  "daytona unavailable".to_string(),
            }
        ]);
    }

    #[tokio::test]
    async fn get_returns_one_matching_sandbox() {
        let docker = info(SandboxProviderKind::Docker, "same-id");
        let registry = registry(vec![
            FakeProvider::new(
                SandboxProviderKind::Docker,
                FakeList::Ok(Vec::new()),
                FakeGet::Found(Box::new(docker.clone())),
            ),
            FakeProvider::new(
                SandboxProviderKind::Daytona,
                FakeList::Ok(Vec::new()),
                FakeGet::Missing,
            ),
        ]);

        assert_eq!(
            registry.get_managed_by_native_id("same-id").await.unwrap(),
            docker
        );
    }

    #[tokio::test]
    async fn get_returns_not_found_when_all_providers_miss() {
        let registry = registry(vec![
            FakeProvider::new(
                SandboxProviderKind::Docker,
                FakeList::Ok(Vec::new()),
                FakeGet::Missing,
            ),
            FakeProvider::new(
                SandboxProviderKind::Daytona,
                FakeList::Ok(Vec::new()),
                FakeGet::Missing,
            ),
        ]);

        let err = registry
            .get_managed_by_native_id("missing")
            .await
            .unwrap_err();

        assert!(matches!(err, SandboxLookupError::NotFound { id } if id == "missing"));
    }

    #[tokio::test]
    async fn get_returns_conflict_when_two_providers_match() {
        let registry = registry(vec![
            FakeProvider::new(
                SandboxProviderKind::Docker,
                FakeList::Ok(Vec::new()),
                FakeGet::Found(Box::new(info(SandboxProviderKind::Docker, "same-id"))),
            ),
            FakeProvider::new(
                SandboxProviderKind::Daytona,
                FakeList::Ok(Vec::new()),
                FakeGet::Found(Box::new(info(SandboxProviderKind::Daytona, "same-id"))),
            ),
        ]);

        let err = registry
            .get_managed_by_native_id("same-id")
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            SandboxLookupError::Conflict { id, providers }
                if id == "same-id"
                    && providers == vec![SandboxProviderKind::Docker, SandboxProviderKind::Daytona]
        ));
    }

    #[tokio::test]
    async fn get_returns_provider_unavailable_when_no_match_and_one_provider_fails() {
        let registry = registry(vec![
            FakeProvider::new(
                SandboxProviderKind::Docker,
                FakeList::Ok(Vec::new()),
                FakeGet::Missing,
            ),
            FakeProvider::new(
                SandboxProviderKind::Daytona,
                FakeList::Ok(Vec::new()),
                FakeGet::Err("daytona unavailable"),
            ),
        ]);

        let err = registry
            .get_managed_by_native_id("maybe-missing")
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            SandboxLookupError::ProviderUnavailable {
                id,
                provider_errors
            } if id == "maybe-missing"
                && provider_errors == vec![SandboxProviderLookupError {
                    provider: SandboxProviderKind::Daytona,
                    message: "daytona unavailable".to_string(),
                }]
        ));
    }
}
