#![allow(unsafe_code)]

use std::sync::{Mutex, OnceLock};

struct Buffer {
    start: *mut u8,
    len: usize,
}

// Safety: after init() we treat the captured argv region as exclusively writable
// by this crate, and serialize writes with the mutex below.
unsafe impl Send for Buffer {}
// Safety: the raw pointer metadata is immutable after capture.
unsafe impl Sync for Buffer {}

static STATE: OnceLock<Mutex<Buffer>> = OnceLock::new();

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn fabro_proctitle_argv_start() -> *mut libc::c_char;
    fn fabro_proctitle_argv_len() -> libc::c_ulong;
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn _NSGetArgv() -> *mut *mut *mut libc::c_char;
    fn _NSGetArgc() -> *mut libc::c_int;
}

/// Capture the argv buffer. Call once early in the process.
#[must_use]
pub fn init() -> usize {
    if let Some(state) = STATE.get() {
        return state.lock().map_or(0, |buffer| buffer.len);
    }

    let Some(buffer) = platform_init() else {
        return 0;
    };
    let len = buffer.len;
    let _ = STATE.set(Mutex::new(buffer));

    STATE
        .get()
        .and_then(|state| state.lock().ok().map(|buffer| buffer.len))
        .unwrap_or(len)
}

/// Overwrite the process title shown by `ps`.
pub fn set(title: &str) {
    let Some(state) = STATE.get() else {
        return;
    };
    let Ok(buffer) = state.lock() else {
        return;
    };
    if buffer.start.is_null() || buffer.len == 0 {
        return;
    }

    // SAFETY: init() captured a writable argv byte range for this process, and
    // the mutex guard above provides exclusive access while we rewrite it.
    let dst = unsafe { std::slice::from_raw_parts_mut(buffer.start, buffer.len) };
    write_title(dst, title.as_bytes());
}

fn write_title(dst: &mut [u8], title: &[u8]) {
    if dst.is_empty() {
        return;
    }

    dst.fill(0);
    let copy_len = title.len().min(dst.len().saturating_sub(1));
    dst[..copy_len].copy_from_slice(&title[..copy_len]);
}

#[cfg(target_os = "linux")]
fn platform_init() -> Option<Buffer> {
    // SAFETY: these symbols are provided by the Linux-only C object compiled in build.rs.
    let start = unsafe { fabro_proctitle_argv_start() };
    // SAFETY: paired with the symbol above.
    let len = unsafe { fabro_proctitle_argv_len() };
    let len = usize::try_from(len).ok()?;
    if start.is_null() || len == 0 {
        return None;
    }

    Some(Buffer {
        start: start.cast(),
        len,
    })
}

#[cfg(target_os = "macos")]
fn platform_init() -> Option<Buffer> {
    // SAFETY: macOS exposes argc/argv through crt_externs for the current process.
    let argc_ptr = unsafe { _NSGetArgc() };
    // SAFETY: paired with _NSGetArgc above.
    let argv_ptr = unsafe { _NSGetArgv() };
    if argc_ptr.is_null() || argv_ptr.is_null() {
        return None;
    }

    // SAFETY: the pointers above are process globals owned by libc.
    let argc = unsafe { *argc_ptr };
    // SAFETY: same as above.
    let argv = unsafe { *argv_ptr };
    if argc <= 0 || argv.is_null() {
        return None;
    }

    // SAFETY: argc > 0 and argv is non-null, so argv[0] and argv[argc - 1] are valid to read.
    let start = unsafe { *argv };
    // SAFETY: same bound check as above.
    let last = unsafe { *argv.add(usize::try_from(argc).ok()?.saturating_sub(1)) };
    if start.is_null() || last.is_null() {
        return None;
    }

    // SAFETY: last points to a C string owned by the process image.
    let last_len = unsafe { libc::strlen(last) };
    // SAFETY: advancing by the string length plus trailing NUL stays within the captured argv span.
    let end = unsafe { last.add(last_len + 1) };
    let len = (end as usize).checked_sub(start as usize)?;
    if len == 0 {
        return None;
    }

    Some(Buffer {
        start: start.cast(),
        len,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_init() -> Option<Buffer> {
    None
}

#[cfg(test)]
mod tests {
    use super::write_title;

    #[test]
    fn write_title_zero_fills_remainder() {
        let mut buffer = [b'x'; 8];
        write_title(&mut buffer, b"fabro");
        assert_eq!(buffer, [b'f', b'a', b'b', b'r', b'o', 0, 0, 0]);
    }

    #[test]
    fn write_title_truncates_to_leave_nul() {
        let mut buffer = [b'x'; 6];
        write_title(&mut buffer, b"toolong");
        assert_eq!(buffer, [b't', b'o', b'o', b'l', b'o', 0]);
    }
}
