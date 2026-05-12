//! Install/API-key validation access to the built-in catalog.
//!
//! Runtime request-serving paths should use a resolved catalog threaded
//! through their state. This module is the explicit hatch for setup flows that
//! need built-in provider/model metadata before project settings are loaded.

use crate::Catalog;

#[must_use]
pub fn catalog() -> &'static Catalog {
    Catalog::builtin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_catalog_is_the_builtin_catalog() {
        assert!(std::ptr::eq(catalog(), Catalog::builtin()));
    }
}
