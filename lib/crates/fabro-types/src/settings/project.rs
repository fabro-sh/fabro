use serde::{Deserialize, Serialize};

fn default_root() -> String {
    ".".to_string()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProjectFabroSettings {
    #[serde(default = "default_root")]
    pub root: String,
}

impl Default for ProjectFabroSettings {
    fn default() -> Self {
        Self {
            root: default_root(),
        }
    }
}
