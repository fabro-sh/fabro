//! Hook configuration runtime settings.

pub use fabro_types::settings::run::{HookDefinition, HookEvent, HookType, TlsMode};
use serde::{Deserialize, Serialize};

/// Top-level hook configuration: a list of hook definitions.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct HookSettings {
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
}

impl HookSettings {
    /// Merge with another config. Concatenates lists; on name collisions,
    /// `other` wins.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        let mut by_name: std::collections::HashMap<String, HookDefinition> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for hook in self.hooks {
            let name = hook.effective_name();
            if !by_name.contains_key(&name) {
                order.push(name.clone());
            }
            by_name.insert(name, hook);
        }
        for hook in other.hooks {
            let name = hook.effective_name();
            if !by_name.contains_key(&name) {
                order.push(name.clone());
            }
            by_name.insert(name, hook);
        }

        let hooks = order
            .into_iter()
            .filter_map(|name| by_name.remove(&name))
            .collect();

        Self { hooks }
    }
}
