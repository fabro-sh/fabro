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
}

impl SandboxProvider {
    /// True only for Local. Used by dry-run to force local execution.
    /// NOT the same as "runs on the host" (Docker is host-adjacent but not
    /// dry-run compatible).
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

impl fmt::Display for SandboxProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Docker => write!(f, "docker"),
            Self::Daytona => write!(f, "daytona"),
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
        assert!("invalid".parse::<SandboxProvider>().is_err());
    }

    #[test]
    fn sandbox_provider_display() {
        assert_eq!(SandboxProvider::Local.to_string(), "local");
        assert_eq!(SandboxProvider::Docker.to_string(), "docker");
        assert_eq!(SandboxProvider::Daytona.to_string(), "daytona");
    }
}
