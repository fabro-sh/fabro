//! Cross-platform idle sleep prevention (No Sleep Till Brooklyn).
//!
//! Call [`guard(true)`] to acquire a sleep inhibitor that prevents the system
//! from idle-sleeping while Fabro is working. The guard is released on drop.

#[cfg(target_os = "macos")]
mod iokit_bindings;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

mod dummy;

use tracing::debug;

/// RAII guard that prevents idle system sleep while held.
pub struct SleepInhibitorGuard {
    _inner: InnerGuard,
}

// Fields are held for their Drop implementations, not read directly.
#[allow(
    dead_code,
    reason = "Enum variants are kept for Drop side effects rather than direct reads."
)]
enum InnerGuard {
    #[cfg(target_os = "macos")]
    MacOS(macos::MacOSSleepInhibitor),
    #[cfg(target_os = "linux")]
    Linux(linux::LinuxSleepInhibitor),
    Dummy(dummy::DummySleepInhibitor),
}

/// Acquire a sleep inhibitor guard.
///
/// If `enabled` is `false`, returns `None` immediately.
/// If `enabled` is `true`, attempts to acquire a platform-specific sleep
/// inhibitor. Falls back to a dummy (no-op) backend if the platform backend
/// is unavailable.
pub fn guard(enabled: bool) -> Option<SleepInhibitorGuard> {
    if !enabled {
        debug!("Sleep inhibitor: disabled by configuration");
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(inner) = macos::MacOSSleepInhibitor::acquire() {
            return Some(SleepInhibitorGuard {
                _inner: InnerGuard::MacOS(inner),
            });
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(inner) = linux::LinuxSleepInhibitor::acquire() {
            return Some(SleepInhibitorGuard {
                _inner: InnerGuard::Linux(inner),
            });
        }
    }

    // Fallback to dummy
    dummy::DummySleepInhibitor::acquire().map(|inner| SleepInhibitorGuard {
        _inner: InnerGuard::Dummy(inner),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_enabled_returns_some() {
        let g = guard(true);
        assert!(g.is_some(), "guard(true) should return Some");
    }

    #[test]
    fn guard_disabled_returns_none() {
        let g = guard(false);
        assert!(g.is_none(), "guard(false) should return None");
    }

    #[test]
    fn guard_drop_does_not_panic() {
        let g = guard(true);
        drop(g);
    }
}
