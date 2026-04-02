#![allow(unsafe_code)]

#[cfg(unix)]
mod flock;
#[cfg(unix)]
mod pre_exec;
mod signal;
mod title;

pub use title::{init as title_init, set as title_set};

pub use signal::process_alive;
#[cfg(unix)]
pub use signal::{sigkill, sigterm, sigterm_process_group};

#[cfg(unix)]
pub use flock::{flock_unlock, try_flock_exclusive};

#[cfg(target_os = "linux")]
pub use pre_exec::pre_exec_pdeathsig;
#[cfg(unix)]
pub use pre_exec::pre_exec_setpgid;
#[cfg(unix)]
pub use pre_exec::pre_exec_setsid;
