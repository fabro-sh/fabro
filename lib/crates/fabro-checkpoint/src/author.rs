use std::fmt::Write;

use fabro_types::settings::InterpString;
use fabro_types::settings::run::GitAuthorLayer;

/// Resolved git author identity for checkpoint commits.
#[derive(Debug, Clone, PartialEq)]
pub struct GitAuthor {
    pub name: String,
    pub email: String,
}

impl Default for GitAuthor {
    fn default() -> Self {
        Self {
            name: "Fabro".into(),
            email: "noreply@fabro.sh".into(),
        }
    }
}

impl GitAuthor {
    /// Create a `GitAuthor` from optional name/email, falling back to defaults.
    pub fn from_options(name: Option<String>, email: Option<String>) -> Self {
        let defaults = Self::default();
        Self {
            name: name.unwrap_or(defaults.name),
            email: email.unwrap_or(defaults.email),
        }
    }

    /// Returns true when this identity matches the default Fabro identity.
    pub fn is_default(&self) -> bool {
        let defaults = Self::default();
        self.name == defaults.name && self.email == defaults.email
    }

    /// Append the Fabro footer (and Co-Authored-By when the author is not the
    /// default identity) to a commit message.
    pub fn append_footer(&self, message: &mut String) {
        message.push_str("\n\u{2692}\u{fe0f} Generated with [Fabro](https://fabro.sh)\n");
        if !self.is_default() {
            let defaults = Self::default();
            let _ = write!(
                message,
                "\nCo-Authored-By: {} <{}>\n",
                defaults.name, defaults.email
            );
        }
    }
}

impl From<&GitAuthorLayer> for GitAuthor {
    fn from(value: &GitAuthorLayer) -> Self {
        Self::from_options(
            value.name.as_ref().map(InterpString::as_source),
            value.email.as_ref().map(InterpString::as_source),
        )
    }
}
