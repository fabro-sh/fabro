#![allow(
    unsafe_code,
    reason = "This crate wraps low-level OS or FFI APIs that require unsafe code."
)]

#[cfg(unix)]
mod flock;
#[cfg(unix)]
mod pre_exec;
mod signal;
mod title;

#[cfg(unix)]
pub use flock::{flock_unlock, try_flock_exclusive};
#[cfg(target_os = "linux")]
pub use pre_exec::pre_exec_pdeathsig;
#[cfg(unix)]
pub use pre_exec::pre_exec_setpgid;
#[cfg(unix)]
pub use pre_exec::pre_exec_setsid;
pub use signal::{process_alive, process_group_alive};
#[cfg(unix)]
pub use signal::{
    sigkill, sigkill_process_group, sigterm, sigterm_process_group, sigusr1, sigusr2,
};
pub use title::{init as title_init, set as title_set};
