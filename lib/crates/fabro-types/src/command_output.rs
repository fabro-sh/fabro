use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}

impl CommandOutputStream {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    #[must_use]
    pub fn log_filename(self) -> &'static str {
        match self {
            Self::Stdout => "stdout.log",
            Self::Stderr => "stderr.log",
        }
    }

    #[must_use]
    pub fn command_log_relative_path(self) -> PathBuf {
        PathBuf::from("command").join(self.log_filename())
    }
}
