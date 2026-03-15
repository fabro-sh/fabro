//! Raw FFI bindings for IOKit power management on macOS.

use core_foundation::string::CFStringRef;

pub type IOPMAssertionID = u32;
pub type IOReturn = i32;

pub const kIOPMAssertionLevelOn: u32 = 255;

extern "C" {
    pub fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        assertion_level: u32,
        reason_for_activity: CFStringRef,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;

    pub fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;
}
