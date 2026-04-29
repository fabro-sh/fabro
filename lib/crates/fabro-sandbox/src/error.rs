#[cfg(feature = "docker")]
use bollard::errors::Error as BollardError;
use fabro_util::error::{collect_causes, render_with_causes};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),

    #[error("{message}")]
    Context {
        message: String,
        #[source]
        source:  Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[cfg(feature = "docker")]
    #[error("Failed to connect to Docker daemon")]
    DockerConnect {
        #[source]
        source: BollardError,
    },

    #[cfg(feature = "docker")]
    #[error("Failed to inspect Docker image {image}")]
    DockerImageInspect {
        image:  String,
        #[source]
        source: BollardError,
    },

    #[cfg(feature = "docker")]
    #[error("Failed to pull Docker image {image}")]
    DockerImagePull {
        image:  String,
        #[source]
        source: BollardError,
    },
}

impl Error {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub fn context(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Context {
            message: message.into(),
            source:  Box::new(source),
        }
    }

    #[cfg(feature = "docker")]
    pub fn docker_connect(source: BollardError) -> Self {
        Self::DockerConnect { source }
    }

    #[cfg(feature = "docker")]
    pub fn docker_image_inspect(image: impl Into<String>, source: BollardError) -> Self {
        Self::DockerImageInspect {
            image: image.into(),
            source,
        }
    }

    #[cfg(feature = "docker")]
    pub fn docker_image_pull(image: impl Into<String>, source: BollardError) -> Self {
        Self::DockerImagePull {
            image: image.into(),
            source,
        }
    }

    pub fn causes(&self) -> Vec<String> {
        collect_causes(self)
    }

    pub fn display_with_causes(&self) -> String {
        render_with_causes(&self.to_string(), &self.causes())
    }
}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
