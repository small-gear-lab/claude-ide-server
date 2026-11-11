//! launchd's socket activation, for macOS.
//!
//! launchd can listen on a socket for a service and start the service at the first connection. It hands the socket over
//! by name, through `launch_activate_socket(3)` (see `man launch`), and not through the environment as systemd does. That
//! call is C, so calling it, and taking ownership of the file descriptors that it returns, needs `unsafe`, which the rest
//! of this workspace forbids. This crate is the one place that has it; what it gives back is safe to use: owned file
//! descriptors.
//!
//! On every other system the crate is empty.

#![cfg(target_os = "macos")]

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

// An `extern` block is `unsafe` since the 2024 edition (the declaration may be wrong, and the compiler cannot tell).
#[allow(unsafe_code)]
unsafe extern "C" {
    /// `int launch_activate_socket(const char *name, int **fds, size_t *cnt)`, from `<launch.h>` (in libSystem).
    fn launch_activate_socket(
        name: *const libc::c_char,
        fds: *mut *mut libc::c_int,
        count: *mut libc::size_t,
    ) -> libc::c_int;
}

/// Why no socket was handed over.
#[derive(Debug)]
pub enum Error {
    /// This process was not started by launchd (`ESRCH`): there is nothing to ask.
    NotManaged,
    /// launchd started it, but its property list has no socket of that name (`ENOENT`).
    NotInPlist,
    /// The call failed for another reason, among them `EALREADY`: the socket of that name was handed over before.
    Failed(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotManaged => f.write_str("the process was not started by launchd"),
            Error::NotInPlist => {
                f.write_str("the property list of the job has no socket of that name")
            }
            Error::Failed(error) => write!(f, "launch_activate_socket failed: {error}"),
        }
    }
}

impl std::error::Error for Error {}

/// The sockets that launchd listens on for the socket named `name` in the `Sockets` dictionary of the property list of
/// this job (a name can stand for several sockets, for example one per address family). They are already listening.
///
/// launchd hands a socket over once: a second call for the same name fails.
pub fn activate_socket(name: &str) -> Result<Vec<OwnedFd>, Error> {
    let name = CString::new(name).map_err(|_| {
        Error::Failed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the name of the socket has a NUL in it",
        ))
    })?;
    let mut fds: *mut libc::c_int = std::ptr::null_mut();
    let mut count: libc::size_t = 0;

    // SAFETY: `name` is a NUL-terminated string that lives through the call, and `fds` and `count` are valid places to
    // write to. The function writes to them only when it returns 0.
    #[allow(unsafe_code)]
    let code = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut count) };
    match code {
        0 => {}
        libc::ESRCH => return Err(Error::NotManaged),
        libc::ENOENT => return Err(Error::NotInPlist),
        other => return Err(Error::Failed(io::Error::from_raw_os_error(other))),
    }
    if fds.is_null() {
        return Ok(Vec::new());
    }

    // SAFETY: on success `fds` points to `count` file descriptors in memory that was allocated with `malloc`, and the
    // descriptors are ours: each is taken once, by `OwnedFd`, which closes it. The array itself is freed once, after
    // the descriptors have been copied out, and not used again.
    #[allow(unsafe_code)]
    let owned = unsafe {
        let owned = (0..count)
            .map(|index| OwnedFd::from_raw_fd(*fds.add(index)))
            .collect();
        libc::free(fds.cast());
        owned
    };
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_job_of_launchd_there_is_no_socket_to_ask_for() {
        // The process of a test is not the job that a property list describes: launchd has nothing for it, whether it says
        // that the process is not managed or that the name is not in the list.
        match activate_socket("claude-ide-server-test") {
            Err(Error::NotManaged | Error::NotInPlist) => {}
            other => panic!("a socket from nowhere: {other:?}"),
        }
    }

    #[test]
    fn a_name_with_a_nul_is_refused_before_launchd_is_asked() {
        assert!(matches!(activate_socket("a\0b"), Err(Error::Failed(_))));
    }
}
