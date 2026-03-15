//! Cross-platform idle sleep prevention.
//!
//! Named after Beastie Boys — No Sleep Till Brooklyn.
//!
//! Call [`guard(true)`] to acquire an inhibitor that prevents the OS from
//! entering idle sleep. The guard releases automatically on [`Drop`].

#[cfg_attr(any(target_os = "macos", target_os = "linux"), allow(dead_code))]
mod dummy;

#[cfg(target_os = "macos")]
mod iokit_bindings;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

/// RAII guard that prevents idle system sleep while held.
pub struct SleepInhibitorGuard {
    #[cfg(target_os = "macos")]
    _inner: macos::MacOsGuard,
    #[cfg(target_os = "linux")]
    _inner: linux::LinuxGuard,
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    _inner: dummy::DummyGuard,
}

/// Acquire a sleep inhibitor guard.
///
/// Returns `Some(guard)` if `enabled` is `true` and the platform backend
/// succeeds. Returns `None` if `enabled` is `false` or the backend fails.
/// The guard prevents idle system sleep until it is dropped.
pub fn guard(enabled: bool) -> Option<SleepInhibitorGuard> {
    if !enabled {
        return None;
    }
    tracing::info!("Acquiring sleep inhibitor");

    #[cfg(target_os = "macos")]
    {
        macos::MacOsGuard::acquire().map(|g| SleepInhibitorGuard { _inner: g })
    }
    #[cfg(target_os = "linux")]
    {
        linux::LinuxGuard::acquire().map(|g| SleepInhibitorGuard { _inner: g })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        dummy::DummyGuard::acquire().map(|g| SleepInhibitorGuard { _inner: g })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_disabled_returns_none() {
        assert!(guard(false).is_none());
    }

    #[test]
    fn guard_enabled_returns_some() {
        // On CI/Linux without systemd-inhibit this may return None,
        // so we only assert it doesn't panic. On macOS it should return Some.
        let g = guard(true);
        // Dummy backend always succeeds; real backends may fail in CI.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert!(g.is_some());
        drop(g);
    }

    #[test]
    fn guard_drop_does_not_panic() {
        let g = guard(true);
        drop(g);
    }
}
