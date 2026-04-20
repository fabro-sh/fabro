#![allow(
    unsafe_code,
    reason = "This crate wraps low-level OS or FFI APIs that require unsafe code."
)]

use core_foundation::base::TCFType;
use tracing::{debug, warn};

use super::iokit_bindings::*;

pub(crate) struct MacOSSleepInhibitor {
    assertion_id: IOPMAssertionID,
}

impl MacOSSleepInhibitor {
    pub(crate) fn acquire() -> Option<Self> {
        let assertion_type = prevent_idle_sleep_type();
        let reason = assertion_reason();
        let mut assertion_id: IOPMAssertionID = kIOPMAssertionIDInvalid;

        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                kIOPMAssertionLevelOn,
                reason.as_concrete_TypeRef(),
                &mut assertion_id,
            )
        };

        if result == kIOReturnSuccess {
            debug!(
                assertion_id,
                "Sleep inhibitor: acquired IOKit power assertion"
            );
            Some(Self { assertion_id })
        } else {
            warn!(
                result,
                "Sleep inhibitor: failed to create IOKit power assertion"
            );
            None
        }
    }
}

impl Drop for MacOSSleepInhibitor {
    fn drop(&mut self) {
        debug!(
            assertion_id = self.assertion_id,
            "Sleep inhibitor: releasing IOKit power assertion"
        );
        let result = unsafe { IOPMAssertionRelease(self.assertion_id) };
        if result != kIOReturnSuccess {
            warn!(
                result,
                "Sleep inhibitor: failed to release IOKit power assertion"
            );
        }
    }
}
