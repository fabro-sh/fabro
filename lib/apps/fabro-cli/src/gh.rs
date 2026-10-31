use fabro_github::GitHubHost;
use tokio::process::Command;
use tracing::debug;

/// Best-effort wrapper around the `gh` CLI.
///
/// All methods degrade gracefully: if `gh` is missing or not authenticated
/// the caller receives `None` / empty results rather than errors.
pub(crate) struct GhCli {
    command: String,
    host:    GitHubHost,
}

impl GhCli {
    /// Attempt to find an authenticated `gh` CLI on PATH.
    ///
    /// Returns `None` if `gh` is not installed or not authenticated
    /// against the configured GitHub host.
    pub(crate) async fn detect(host: &GitHubHost) -> Option<Self> {
        Self::detect_with_command("gh", host).await
    }

    async fn detect_with_command(command: &str, host: &GitHubHost) -> Option<Self> {
        let version = Command::new(command).arg("--version").output().await;
        let Ok(output) = version else {
            debug!("gh CLI not found on PATH");
            return None;
        };
        if !output.status.success() {
            debug!("gh --version failed");
            return None;
        }

        let auth = Command::new(command)
            .args(["auth", "status", "--hostname", host.hostname()])
            .output()
            .await;
        match auth {
            Ok(o) if o.status.success() => {
                debug!("gh CLI available and authenticated for {}", host.hostname());
                Some(Self {
                    command: command.to_string(),
                    host:    host.clone(),
                })
            }
            _ => {
                debug!("gh is not authenticated for {}", host.hostname());
                None
            }
        }
    }

    /// Return the login of the authenticated GitHub user, or `None` on failure.
    pub(crate) async fn authenticated_user(&self) -> Option<String> {
        let output = Command::new(&self.command)
            .args([
                "api",
                "--hostname",
                self.host.hostname(),
                "/user",
                "--jq",
                ".login",
            ])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            debug!("gh api /user failed");
            return None;
        }
        let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if login.is_empty() { None } else { Some(login) }
    }

    /// List GitHub organizations the authenticated user is an admin of.
    ///
    /// Returns an empty vec on any failure (network, parse, etc.).
    pub(crate) async fn list_admin_orgs(&self) -> Vec<String> {
        let output = Command::new(&self.command)
            .args([
                "api",
                "--hostname",
                self.host.hostname(),
                "--paginate",
                "/user/memberships/orgs",
                "--jq",
                r#".[] | select(.role == "admin" and .state == "active") | .organization.login"#,
            ])
            .output()
            .await;
        let Ok(output) = output else {
            debug!("gh api /user/memberships/orgs failed to execute");
            return Vec::new();
        };
        if !output.status.success() {
            debug!(
                "gh api /user/memberships/orgs exited with {}",
                output.status
            );
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detect_returns_none_when_gh_is_missing() {
        let result = GhCli::detect_with_command(
            "fabro-test-gh-that-should-not-exist",
            &GitHubHost::default(),
        )
        .await;
        assert!(result.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "unit test stages a fake gh script and reads its argument log with sync std::fs"
    )]
    async fn gh_invocations_target_the_configured_host() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args.log");
        let script = dir.path().join("gh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        for (web_url, hostname) in [
            ("https://github.com", "github.com"),
            ("https://ghe.example.com:8443", "ghe.example.com"),
        ] {
            let host = GitHubHost::new(web_url, None).unwrap();
            let gh = GhCli::detect_with_command(script.to_str().unwrap(), &host)
                .await
                .unwrap();
            gh.authenticated_user().await;
            gh.list_admin_orgs().await;

            let logged = std::fs::read_to_string(&log).unwrap();
            std::fs::remove_file(&log).unwrap();
            let lines = logged.lines().collect::<Vec<_>>();
            assert_eq!(lines[0], "--version");
            assert_eq!(lines[1], format!("auth status --hostname {hostname}"));
            assert!(lines[2].starts_with(&format!("api --hostname {hostname} /user")));
            assert!(lines[3].starts_with(&format!("api --hostname {hostname} --paginate")));
        }
    }
}
