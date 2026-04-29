use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter(String);

impl LogFilter {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        if value.chars().any(char::is_whitespace) {
            anyhow::bail!("filter must not contain whitespace");
        }

        EnvFilter::builder()
            .parse(value)
            .map(|_| Self(value.to_owned()))
            .map_err(Into::into)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for LogFilter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for LogFilter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(|err| {
            D::Error::custom(format!("invalid server.logging.level `{value}`: {err}"))
        })
    }
}
