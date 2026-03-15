use super::iokit_bindings::*;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;

/// macOS sleep inhibitor using IOKit power assertions.
pub(crate) struct MacOsGuard {
    assertion_id: IOPMAssertionID,
}

impl MacOsGuard {
    pub(crate) fn acquire() -> Option<Self> {
        let assertion_type = CFString::from_static_string("PreventUserIdleSystemSleep");
        let reason = CFString::new("fabro workflow in progress");

        let mut assertion_id: IOPMAssertionID = 0;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                kIOPMAssertionLevelOn,
                reason.as_concrete_TypeRef(),
                &mut assertion_id,
            )
        };

        if result != 0 {
            tracing::warn!(
                io_return = result,
                "Failed to create IOKit power assertion for sleep prevention"
            );
            return None;
        }

        tracing::debug!(
            assertion_id,
            "Sleep inhibitor: macOS IOKit assertion acquired"
        );
        Some(Self { assertion_id })
    }
}

impl Drop for MacOsGuard {
    fn drop(&mut self) {
        let result = unsafe { IOPMAssertionRelease(self.assertion_id) };
        if result != 0 {
            tracing::warn!(
                io_return = result,
                assertion_id = self.assertion_id,
                "Failed to release IOKit power assertion"
            );
        } else {
            tracing::debug!(
                assertion_id = self.assertion_id,
                "Sleep inhibitor: macOS IOKit assertion released"
            );
        }
    }
}
