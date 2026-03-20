use std::fmt;
use std::str::FromStr;

/// Sandbox provider for agent tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxProvider {
    /// Run tools on the local host (default)
    #[default]
    Local,
    /// Run tools inside a Docker container
    Docker,
    /// Run tools inside a Daytona cloud sandbox
    Daytona,
    /// Run tools inside an exe.dev VM
    #[cfg(feature = "exedev")]
    Exe,
    /// Run tools on a user-provided SSH host
    Ssh,
}

impl SandboxProvider {
    #[must_use]
    pub fn is_remote(&self) -> bool {
        match self {
            Self::Daytona => true,
            #[cfg(feature = "exedev")]
            Self::Exe => true,
            Self::Ssh => true,
            _ => false,
        }
    }
}

impl fmt::Display for SandboxProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Docker => write!(f, "docker"),
            Self::Daytona => write!(f, "daytona"),
            #[cfg(feature = "exedev")]
            Self::Exe => write!(f, "exe"),
            Self::Ssh => write!(f, "ssh"),
        }
    }
}

impl FromStr for SandboxProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "docker" => Ok(Self::Docker),
            "daytona" => Ok(Self::Daytona),
            #[cfg(feature = "exedev")]
            "exe" => Ok(Self::Exe),
            "ssh" => Ok(Self::Ssh),
            other => Err(format!("unknown sandbox provider: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SandboxProvider;

    #[test]
    fn sandbox_provider_default_is_local() {
        assert_eq!(SandboxProvider::default(), SandboxProvider::Local);
    }

    #[test]
    fn sandbox_provider_from_str() {
        assert_eq!(
            "local".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Local
        );
        assert_eq!(
            "docker".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Docker
        );
        assert_eq!(
            "daytona".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Daytona
        );
        assert_eq!(
            "LOCAL".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Local
        );
        #[cfg(feature = "exedev")]
        {
            assert_eq!(
                "exe".parse::<SandboxProvider>().unwrap(),
                SandboxProvider::Exe
            );
            assert_eq!(
                "EXE".parse::<SandboxProvider>().unwrap(),
                SandboxProvider::Exe
            );
        }
        assert_eq!(
            "ssh".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Ssh
        );
        assert_eq!(
            "SSH".parse::<SandboxProvider>().unwrap(),
            SandboxProvider::Ssh
        );
        assert!("invalid".parse::<SandboxProvider>().is_err());
    }

    /// Bug: run.rs uses `!is_remote()` to decide worktree eligibility, but
    /// Docker is non-remote yet its sandbox creation arm never creates the
    /// worktree.  This causes worktree variables to be set but the
    /// branch/worktree never created, leading to checkpoint failures.
    #[test]
    fn docker_should_not_be_eligible_for_local_worktree_strategy() {
        // All non-remote providers must have a sandbox creation arm that
        // handles worktree setup.  Currently only Local does, so Docker
        // being non-remote is a bug.
        assert!(
            SandboxProvider::Docker.is_remote(),
            "Docker.is_remote() returns false, making it eligible for LocalWorktree strategy, \
             but the Docker sandbox creation arm never creates the git worktree"
        );
    }

    #[test]
    fn sandbox_provider_display() {
        assert_eq!(SandboxProvider::Local.to_string(), "local");
        assert_eq!(SandboxProvider::Docker.to_string(), "docker");
        assert_eq!(SandboxProvider::Daytona.to_string(), "daytona");
        #[cfg(feature = "exedev")]
        assert_eq!(SandboxProvider::Exe.to_string(), "exe");
        assert_eq!(SandboxProvider::Ssh.to_string(), "ssh");
    }
}
