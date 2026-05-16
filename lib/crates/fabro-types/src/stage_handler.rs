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
pub enum StageHandler {
    Start,
    Exit,
    Agent,
    Prompt,
    Command,
    Human,
    Conditional,
    Parallel,
    #[serde(rename = "parallel.fan_in")]
    #[strum(serialize = "parallel.fan_in")]
    ParallelFanIn,
    #[serde(rename = "stack.manager_loop")]
    #[strum(serialize = "stack.manager_loop")]
    StackManagerLoop,
    Wait,
}

impl StageHandler {
    #[must_use]
    pub fn from_handler_type(raw: Option<&str>) -> Self {
        match raw.unwrap_or("agent") {
            "start" => Self::Start,
            "exit" => Self::Exit,
            "prompt" => Self::Prompt,
            "command" | "tool" => Self::Command,
            "human" => Self::Human,
            "conditional" => Self::Conditional,
            "parallel" => Self::Parallel,
            "parallel.fan_in" => Self::ParallelFanIn,
            "stack.manager_loop" => Self::StackManagerLoop,
            "wait" => Self::Wait,
            _ => Self::Agent,
        }
    }
}
