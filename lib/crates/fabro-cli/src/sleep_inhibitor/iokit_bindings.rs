#![allow(
    non_upper_case_globals,
    dead_code,
    reason = "FFI bindings preserve IOKit naming and include symbols referenced only on macOS."
)]

use core_foundation::string::{CFString, CFStringRef};

// IOKit power management assertion types
pub(super) type IOPMAssertionID = u32;
pub(super) const kIOPMAssertionIDInvalid: IOPMAssertionID = 0;

// IOReturn type
pub(super) type IOReturn = i32;
pub(super) const kIOReturnSuccess: IOReturn = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub(super) fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        assertion_level: u32,
        reason_for_activity: CFStringRef,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;

    pub(super) fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;
}

// Assertion level
pub(super) const kIOPMAssertionLevelOn: u32 = 255;

/// Create the CFString for "PreventUserIdleSystemSleep".
pub(super) fn prevent_idle_sleep_type() -> CFString {
    CFString::new("PreventUserIdleSystemSleep")
}

/// Create a CFString reason.
pub(super) fn assertion_reason() -> CFString {
    CFString::new("Fabro workflow running")
}
