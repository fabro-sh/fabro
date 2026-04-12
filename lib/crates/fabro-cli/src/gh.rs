use tokio::process::Command;
use tracing::debug;

/// Best-effort wrapper around the `gh` CLI.
///
/// All methods degrade gracefully: if `gh` is missing or not authenticated
/// the caller receives `None` / empty results rather than errors.
pub(crate) struct GhCli {
    _private: (),
}

impl GhCli {
    /// Attempt to find an authenticated `gh` CLI on PATH.
    ///
    /// Returns `None` if `gh` is not installed or not authenticated
    /// against github.com.
    pub(crate) async fn detect() -> Option<Self> {
        let version = Command::new("gh").arg("--version").output().await;
        let Ok(output) = version else {
            debug!("gh CLI not found on PATH");
            return None;
        };
        if !output.status.success() {
            debug!("gh --version failed");
            return None;
        }

        let auth = Command::new("gh")
            .args(["auth", "status", "--hostname", "github.com"])
            .output()
            .await;
        match auth {
            Ok(o) if o.status.success() => {
                debug!("gh CLI available and authenticated for github.com");
                Some(Self { _private: () })
            }
            _ => {
                debug!("gh is not authenticated for github.com");
                None
            }
        }
    }

    /// Return the login of the authenticated GitHub user, or `None` on failure.
    pub(crate) async fn authenticated_user(&self) -> Option<String> {
        let output = Command::new("gh")
            .args(["api", "--hostname", "github.com", "/user", "--jq", ".login"])
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
        let output = Command::new("gh")
            .args([
                "api",
                "--hostname",
                "github.com",
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
    async fn detect_does_not_panic() {
        // Validates graceful degradation — in CI where gh may not be installed
        // this returns None without panicking.
        let _result = GhCli::detect().await;
    }
}
