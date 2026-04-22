use strum::{Display, EnumString};

/// Sandbox provider for agent tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
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
