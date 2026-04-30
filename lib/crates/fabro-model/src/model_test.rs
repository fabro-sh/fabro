use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ModelTestMode {
    #[default]
    Basic,
    Deep,
}

impl ModelTestMode {
    #[must_use]
    pub const fn timeout_secs(self) -> u64 {
        match self {
            Self::Basic => 30,
            Self::Deep => 90,
        }
    }
}
