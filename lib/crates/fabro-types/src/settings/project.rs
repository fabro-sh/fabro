use serde::{Deserialize, Serialize};

fn default_root() -> String {
    ".".to_string()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProjectSettings {
    #[serde(default = "default_root")]
    pub root: String,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            root: default_root(),
        }
    }
}
