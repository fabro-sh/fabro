use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr, VariantArray, VariantNames};

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
    VariantArray,
    VariantNames,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum LlmBackend {
    Api,
    Cli,
    Acp,
}

impl LlmBackend {
    #[must_use]
    pub fn expected_values() -> String {
        <Self as VariantNames>::VARIANTS.join(", ")
    }
}
