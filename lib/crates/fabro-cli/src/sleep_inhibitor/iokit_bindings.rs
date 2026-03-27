#![allow(non_upper_case_globals, dead_code)]

use core_foundation::string::CFString;

// IOKit power management assertion types
pub type IOPMAssertionID = u32;
pub const kIOPMAssertionIDInvalid: IOPMAssertionID = 0;

// IOReturn type
pub type IOReturn = i32;
pub const kIOReturnSuccess: IOReturn = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOPMAssertionCreateWithName(
        assertion_type: core_foundation::string::CFStringRef,
        assertion_level: u32,
        reason_for_activity: core_foundation::string::CFStringRef,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;

    pub fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;
}

// Assertion level
pub const kIOPMAssertionLevelOn: u32 = 255;

/// Create the CFString for "PreventUserIdleSystemSleep".
pub fn prevent_idle_sleep_type() -> CFString {
    CFString::new("PreventUserIdleSystemSleep")
}

/// Create a CFString reason.
pub fn assertion_reason() -> CFString {
    CFString::new("Fabro workflow running")
}
