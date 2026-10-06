//! Local subprocess ownership and capture, plus OS process primitives.
//! Finite commands use [`capture`]; command and credential policy stay with
//! callers. Process ownership remains private to capture.

#![allow(
    unsafe_code,
    reason = "This crate wraps low-level OS or FFI APIs that require unsafe code."
)]

#[cfg(unix)]
mod flock;
#[cfg(unix)]
mod pre_exec;
#[cfg(unix)]
mod process_lock;
mod signal;
mod title;

#[cfg(unix)]
pub use flock::{flock_unlock, try_flock_exclusive, try_flock_shared};
#[cfg(target_os = "linux")]
pub use pre_exec::pre_exec_pdeathsig;
#[cfg(unix)]
pub use pre_exec::pre_exec_setpgid;
#[cfg(unix)]
pub use pre_exec::pre_exec_setsid;
#[cfg(unix)]
pub use process_lock::{ProcessLock, stop_lock_holder};
pub use signal::{process_exists, process_group_alive, process_running, process_running_strict};
#[cfg(unix)]
pub use signal::{
    sigkill, sigkill_process_group, sigterm, sigterm_process_group, sigusr1, sigusr2,
};
pub use title::{init as title_init, set as title_set};

mod process;
pub use process::{CapturedOutput, ProcessError, capture};
