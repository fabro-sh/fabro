//! The validated effective repository set for one run's GitHub access.
//!
//! [`GitHubRepositoryAccess`] is the single value both server preflight and
//! workflow initialization construct from the run origin, the declared
//! additional repositories, and the resolved shared permissions — so the two
//! paths cannot disagree about which repositories a run's `GITHUB_TOKEN`
//! covers. It carries no token or key material.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context as _, bail};
use fabro_types::settings::run::RunIntegrationsGithubSettings;
use fabro_types::{GitHubHost, GitHubRepositorySlug};
use futures::stream::{self, StreamExt as _};

use crate::{GitHubAppCredentials, HttpClient, InstallationLookup, InstallationToken};

/// Keep GitHub installation lookups bounded while avoiding one network round
/// trip at a time for large declared repository sets.
const INSTALLATION_LOOKUP_CONCURRENCY: usize = 4;

/// The validated effective repository set for a run: the primary origin
/// repository plus zero or more distinct additional repositories, all with
/// one shared owner, and the shared permission map that scopes the token.
///
/// Secret-free by construction: `Debug` may render everywhere the run
/// pipeline logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepositoryAccess {
    primary:     GitHubRepositorySlug,
    /// Sorted, deduplicated, primary excluded.
    additional:  Vec<GitHubRepositorySlug>,
    permissions: HashMap<String, String>,
}

impl GitHubRepositoryAccess {
    /// Build the effective access request.
    ///
    /// Returns `Ok(None)` when no origin URL is available and no additional
    /// repositories are declared — the legacy "nothing to scope" state whose
    /// handling stays with the caller. Every declared-additional invariant is
    /// enforced here:
    ///
    /// - a declared additional set requires a GitHub origin,
    /// - the additional set cannot contain the primary repository,
    /// - every additional repository shares the primary's owner
    ///   (case-insensitive) because one App installation covers one account,
    /// - a declared additional set requires interpolated permissions with
    ///   `contents = "read"` or `contents = "write"`.
    pub fn new(
        host: &GitHubHost,
        origin_url: Option<&str>,
        additional_repositories: &BTreeSet<GitHubRepositorySlug>,
        permissions: HashMap<String, String>,
    ) -> anyhow::Result<Option<Self>> {
        let origin_url = origin_url.map(str::trim).filter(|url| !url.is_empty());
        let Some(origin_url) = origin_url else {
            if additional_repositories.is_empty() {
                return Ok(None);
            }
            bail!(
                "run.integrations.github.additional_repositories requires a GitHub run origin; \
                 this run has no repository origin URL"
            );
        };

        let normalized = crate::normalize_repo_origin_url(origin_url);
        let (owner, repo) = crate::parse_github_owner_repo(host, &normalized)
            .context("parsing GitHub origin for repository access")?;
        let Some(primary) = GitHubRepositorySlug::try_new(&format!("{owner}/{repo}")) else {
            bail!("run origin does not name a valid GitHub `owner/repository`: {owner}/{repo}");
        };

        if !additional_repositories.is_empty() {
            validate_additional_permissions(&permissions)?;
        }

        let mut additional = Vec::with_capacity(additional_repositories.len());
        for slug in additional_repositories {
            if *slug == primary {
                bail!(
                    "run.integrations.github.additional_repositories must not repeat the run \
                     origin repository `{primary}` — the origin is always included"
                );
            }
            if !slug.same_owner(&primary) {
                bail!(
                    "additional repository `{slug}` has owner `{}` but the run origin `{primary}` \
                     has owner `{}`; all repositories must share one owner because one GitHub App \
                     installation covers one account",
                    slug.owner(),
                    primary.owner()
                );
            }
            additional.push(slug.clone());
        }

        Ok(Some(Self {
            primary,
            additional,
            permissions,
        }))
    }

    #[must_use]
    pub fn primary(&self) -> &GitHubRepositorySlug {
        &self.primary
    }

    /// Every repository in the effective set, primary first, then the
    /// additional repositories in their deterministic sorted order.
    #[must_use]
    pub fn targets(&self) -> Vec<&GitHubRepositorySlug> {
        std::iter::once(&self.primary)
            .chain(self.additional.iter())
            .collect()
    }

    /// Project each validated slug to its repository-name component for the
    /// installation-token mint request, which accepts names within the
    /// selected installation. Every target shares the primary's owner, so the
    /// projection loses nothing.
    #[must_use]
    pub fn repository_names(&self) -> Vec<String> {
        self.targets()
            .into_iter()
            .map(|slug| slug.repo().to_string())
            .collect()
    }

    #[must_use]
    pub fn owner(&self) -> &str {
        self.primary.owner()
    }

    #[must_use]
    pub fn permissions(&self) -> &HashMap<String, String> {
        &self.permissions
    }

    pub fn permissions_json(&self) -> anyhow::Result<serde_json::Value> {
        serde_json::to_value(&self.permissions).context("serializing GitHub permissions")
    }

    #[must_use]
    pub fn has_additional_repositories(&self) -> bool {
        !self.additional.is_empty()
    }

    /// Resolve every target's App installation and require one shared
    /// installation ID, so a repository the App cannot see — or one that
    /// resolves to a different installation — is named before any token is
    /// minted. Targets are checked in deterministic primary-first order.
    async fn resolve_shared_installation_with_jwt(
        &self,
        client: &impl HttpClient,
        jwt: &str,
        base_url: &str,
    ) -> anyhow::Result<u64> {
        let targets: Vec<GitHubRepositorySlug> = self.targets().into_iter().cloned().collect();
        let mut lookups = stream::iter(targets.into_iter().map(|slug| async move {
            let lookup =
                crate::lookup_installation(client, jwt, base_url, slug.owner(), slug.repo())
                    .await
                    .with_context(|| format!("looking up the GitHub App installation for {slug}"));
            (slug, lookup)
        }))
        .buffered(INSTALLATION_LOOKUP_CONCURRENCY);

        let mut shared: Option<(u64, GitHubRepositorySlug)> = None;
        while let Some((slug, lookup)) = lookups.next().await {
            let lookup = lookup?;
            let id = match lookup {
                InstallationLookup::Found(id) => id,
                InstallationLookup::NotFound => bail!(
                    "the GitHub App installation cannot see repository {slug}; add it to the \
                     installation's repository access"
                ),
                InstallationLookup::Failed(status) => bail!(
                    "unexpected status {status} looking up the GitHub App installation for {slug}"
                ),
            };
            match &shared {
                None => shared = Some((id, slug)),
                Some((shared_id, first)) if *shared_id != id => bail!(
                    "repository {slug} belongs to GitHub App installation {id} but {first} \
                     belongs to installation {shared_id}; all repositories must share one \
                     installation"
                ),
                Some(_) => {}
            }
        }
        let (id, _) = shared.expect("the effective repository set always contains the primary");
        Ok(id)
    }

    /// Resolve every target to one installation, then mint one token scoped
    /// to this exact repository set without looking up the primary twice.
    pub(crate) async fn mint_installation_token(
        &self,
        creds: &GitHubAppCredentials,
        client: &impl HttpClient,
        base_url: &str,
    ) -> anyhow::Result<InstallationToken> {
        let jwt = crate::sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
        let installation_id = self
            .resolve_shared_installation_with_jwt(client, &jwt, base_url)
            .await
            .context("resolving the shared GitHub App installation")?;
        crate::mint_installation_token_for_id_with_jwt(
            client,
            &jwt,
            installation_id,
            &self.repository_names(),
            base_url,
            self.permissions_json()?,
        )
        .await
    }
}

/// A non-empty additional set needs a token that can reach repository
/// contents. Configuration resolution already checked literal values; this
/// is the runtime re-check after `{{ vars.* }}` interpolation.
fn validate_additional_permissions(permissions: &HashMap<String, String>) -> anyhow::Result<()> {
    let Some(contents) = permissions.get("contents") else {
        bail!(
            "run.integrations.github.additional_repositories requires the `contents` permission \
             (`read` or `write`)"
        );
    };
    if !RunIntegrationsGithubSettings::contents_permission_allows_repository_access(contents) {
        bail!(
            "run.integrations.github.additional_repositories requires `contents = \"read\"` or \
             `contents = \"write\"`, got `{contents}`"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slugs(values: &[&str]) -> BTreeSet<GitHubRepositorySlug> {
        values
            .iter()
            .map(|value| value.parse().expect("test slug should parse"))
            .collect()
    }

    fn contents_read() -> HashMap<String, String> {
        HashMap::from([("contents".to_string(), "read".to_string())])
    }

    fn access(
        origin: &str,
        additional: &[&str],
        permissions: HashMap<String, String>,
    ) -> anyhow::Result<Option<GitHubRepositoryAccess>> {
        GitHubRepositoryAccess::new(
            &GitHubHost::default(),
            Some(origin),
            &slugs(additional),
            permissions,
        )
    }

    #[test]
    fn https_and_both_ssh_origin_forms_normalize_to_the_same_primary() {
        let origins = [
            "https://github.com/fabro-sh/fabro.git",
            "git@github.com:fabro-sh/fabro.git",
            "ssh://git@github.com/fabro-sh/fabro.git",
            "https://github.com/fabro-sh/fabro",
        ];
        for origin in origins {
            let access = access(origin, &[], HashMap::new())
                .expect(origin)
                .expect("origin should produce an access value");
            assert_eq!(access.primary().to_string(), "fabro-sh/fabro", "{origin}");
        }
    }

    #[test]
    fn no_origin_and_no_additional_repositories_is_none() {
        let access = GitHubRepositoryAccess::new(
            &GitHubHost::default(),
            None,
            &BTreeSet::new(),
            HashMap::new(),
        )
        .unwrap();
        assert!(access.is_none());

        let blank = GitHubRepositoryAccess::new(
            &GitHubHost::default(),
            Some("  "),
            &BTreeSet::new(),
            HashMap::new(),
        )
        .unwrap();
        assert!(blank.is_none());
    }

    #[test]
    fn additional_repositories_require_an_origin() {
        let err = GitHubRepositoryAccess::new(
            &GitHubHost::default(),
            None,
            &slugs(&["fabro-sh/keystone"]),
            contents_read(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("requires a GitHub run origin"),
            "{err:#}"
        );
    }

    #[test]
    fn additional_repositories_require_a_github_origin() {
        let err = access(
            "https://gitlab.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            contents_read(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("repository access"), "{err:#}");
    }

    #[test]
    fn rejects_primary_duplication_regardless_of_url_spelling_or_case() {
        let origins = [
            "https://github.com/Fabro-SH/Fabro.git",
            "git@github.com:fabro-sh/fabro.git",
            "ssh://git@github.com/fabro-sh/fabro",
        ];
        for origin in origins {
            let err = access(origin, &["fabro-sh/FABRO"], contents_read()).unwrap_err();
            assert!(
                err.to_string().contains("must not repeat the run origin"),
                "{origin}: {err:#}"
            );
        }
    }

    #[test]
    fn rejects_an_additional_repository_with_a_different_owner() {
        let err = access(
            "https://github.com/fabro-sh/fabro",
            &["lithoscomputer/conveyor"],
            contents_read(),
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("lithoscomputer/conveyor"), "{message}");
        assert!(message.contains("share one owner"), "{message}");
    }

    #[test]
    fn rejects_a_declared_set_without_a_contents_permission() {
        let missing = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            HashMap::new(),
        )
        .unwrap_err();
        assert!(
            missing.to_string().contains("`contents` permission"),
            "{missing:#}"
        );

        let wrong_level = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            HashMap::from([("contents".to_string(), "admin".to_string())]),
        )
        .unwrap_err();
        assert!(
            wrong_level.to_string().contains("got `admin`"),
            "{wrong_level:#}"
        );
    }

    #[test]
    fn targets_retain_every_full_slug_exactly_once_primary_first() {
        let access = access(
            "git@github.com:fabro-sh/fabro.git",
            &["fabro-sh/keystone", "fabro-sh/arc"],
            contents_read(),
        )
        .unwrap()
        .unwrap();

        let targets: Vec<String> = access.targets().iter().map(ToString::to_string).collect();
        assert_eq!(targets, vec![
            "fabro-sh/fabro",
            "fabro-sh/arc",
            "fabro-sh/keystone",
        ]);
        assert_eq!(access.repository_names(), vec!["fabro", "arc", "keystone"]);
        assert_eq!(access.owner(), "fabro-sh");
        assert!(access.has_additional_repositories());
    }

    #[test]
    fn debug_output_contains_only_repositories_and_permissions() {
        let access = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/arc"],
            contents_read(),
        )
        .unwrap()
        .unwrap();

        let rendered = format!("{access:?}");
        assert!(rendered.contains("fabro-sh"), "{rendered}");
        assert!(rendered.contains("contents"), "{rendered}");
        // The value carries no token or key material by construction; its
        // fields are exactly the repository slugs and the permission map.
        assert!(!rendered.to_lowercase().contains("token"), "{rendered}");
        assert!(!rendered.to_lowercase().contains("key"), "{rendered}");
    }

    #[tokio::test]
    async fn resolve_shared_installation_names_the_invisible_repository() {
        use crate::HttpMethod;
        use crate::tests_mock::{MockHttpClient, test_rsa_key};

        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/fabro/installation",
                200,
                r#"{"id": 7}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/keystone/installation",
                404,
                "{}",
            );
        let creds = GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: test_rsa_key().to_string(),
            slug:            None,
        };
        let access = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            contents_read(),
        )
        .unwrap()
        .unwrap();

        let err = access
            .resolve_shared_installation_with_jwt(
                &mock,
                &crate::sign_app_jwt(&creds.app_id, &creds.private_key_pem).unwrap(),
                "",
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("fabro-sh/keystone"), "{message}");
        assert!(message.contains("cannot see"), "{message}");
    }

    #[tokio::test]
    async fn resolve_shared_installation_requires_one_installation_id() {
        use crate::HttpMethod;
        use crate::tests_mock::{MockHttpClient, test_rsa_key};

        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/fabro/installation",
                200,
                r#"{"id": 7}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/keystone/installation",
                200,
                r#"{"id": 8}"#,
            );
        let creds = GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: test_rsa_key().to_string(),
            slug:            None,
        };
        let access = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            contents_read(),
        )
        .unwrap()
        .unwrap();

        let err = access
            .resolve_shared_installation_with_jwt(
                &mock,
                &crate::sign_app_jwt(&creds.app_id, &creds.private_key_pem).unwrap(),
                "",
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("installation 8"), "{message}");
        assert!(message.contains("fabro-sh/keystone"), "{message}");
    }

    #[tokio::test]
    async fn resolve_shared_installation_returns_the_shared_id() {
        use crate::HttpMethod;
        use crate::tests_mock::{MockHttpClient, test_rsa_key};

        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/fabro/installation",
                200,
                r#"{"id": 7}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/keystone/installation",
                200,
                r#"{"id": 7}"#,
            );
        let creds = GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: test_rsa_key().to_string(),
            slug:            None,
        };
        let access = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            contents_read(),
        )
        .unwrap()
        .unwrap();

        let id = access
            .resolve_shared_installation_with_jwt(
                &mock,
                &crate::sign_app_jwt(&creds.app_id, &creds.private_key_pem).unwrap(),
                "",
            )
            .await
            .unwrap();
        assert_eq!(id, 7);
    }

    #[tokio::test]
    async fn access_mint_reuses_the_resolved_installation_id() {
        use crate::HttpMethod;
        use crate::tests_mock::{MockHttpClient, test_rsa_key};

        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/fabro/installation",
                200,
                r#"{"id": 7}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/fabro-sh/keystone/installation",
                200,
                r#"{"id": 7}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/7/access_tokens",
                201,
                r#"{"token":"scoped","expires_at":"2099-01-01T00:00:00Z"}"#,
            )
            .with_req_body(
                r#"{"repositories":["fabro","keystone"],"permissions":{"contents":"read"}}"#,
            );
        let creds = GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: test_rsa_key().to_string(),
            slug:            None,
        };
        let access = access(
            "https://github.com/fabro-sh/fabro",
            &["fabro-sh/keystone"],
            contents_read(),
        )
        .unwrap()
        .unwrap();

        let token = access
            .mint_installation_token(&creds, &mock, "")
            .await
            .unwrap();

        assert_eq!(token.token, "scoped");
        assert_eq!(
            mock.request_count(),
            3,
            "each repository should be looked up once before the mint"
        );
    }
}
