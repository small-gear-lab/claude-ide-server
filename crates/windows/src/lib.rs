//! The calls to Windows that the server needs, and that need `unsafe`.
//!
//! Three things: whether a process is alive (the lock files and the records of a server that died are cleaned up by it),
//! who the user is (the name of the named pipe has the user in it, so that two users of one machine do not meet), and a
//! named pipe that only that user can reach (the equivalent of a Unix socket with mode 0600). What they give back is safe
//! to use: a `bool`, a `String`, a pipe of `tokio`.
//!
//! On every other system the crate is empty.

#![cfg(windows)]

use std::ffi::{OsStr, c_void};
use std::io;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The exit code that `GetExitCodeProcess` gives for a process that has not ended (`STILL_ACTIVE`).
const STILL_ACTIVE: u32 = 259;

/// A handle that is closed when it goes out of scope.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and is closed only here, once: `Handle` is made from a handle that the system gave
        // and is not copied.
        #[allow(unsafe_code)]
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Whether a process with this id exists on this machine (the equivalent of `kill(pid, 0)`, which Claude Code uses to
/// see if the process of an editor is alive).
pub fn process_is_alive(pid: u32) -> bool {
    // SAFETY: `OpenProcess` takes only numbers, and gives a handle or null.
    #[allow(unsafe_code)]
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // A process that belongs to someone else, and cannot be opened, is there all the same.
        return io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }
    let handle = Handle(handle);
    let mut code = 0u32;
    // SAFETY: `handle` is a valid handle with the right to query, and `code` is a place to write to.
    #[allow(unsafe_code)]
    let read = unsafe { GetExitCodeProcess(handle.0, &mut code) };
    // A process that has ended may still be open (somebody holds a handle to it): it is not alive.
    read != 0 && code == STILL_ACTIVE
}

/// The security identifier (SID) of the user of this process, as a string such as `S-1-5-21-…-1001`. It tells users of one
/// machine apart, in the name of the pipe, and says whom the pipe is for.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` gives a pseudo handle that needs no closing, and `token` is a place to write to.
    #[allow(unsafe_code)]
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);

    // The size first, then the information (the first call is meant to fail for want of room).
    let mut needed = 0u32;
    // SAFETY: a null buffer of size 0 is how the size is asked for; `needed` is a place to write to.
    #[allow(unsafe_code)]
    unsafe {
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // `u64`s, so that the buffer is aligned for what `TOKEN_USER` holds (pointers).
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` has at least `needed` bytes, as the first call said, and is aligned for `TOKEN_USER`.
    #[allow(unsafe_code)]
    let got = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if got == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: on success `buffer` holds a `TOKEN_USER` whose `Sid` points into the buffer, which lives through the call;
    // `text` is a place to write to. The string that the call allocates is read once and freed once, below, and is not
    // used after that.
    #[allow(unsafe_code)]
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// An instance of the named pipe `name` (a name such as `\\.\pipe\claude-ide-server-<sid>`), which only the user of this
/// process can open, and which a client on another machine cannot reach.
///
/// With `first` it is the first instance of that name, and the call fails with `PermissionDenied` if the name is in use:
/// another server has it (a name that nobody has is free, since a pipe goes with its last handle). The instances after the
/// first (`first` false) are the ones that wait for the next client, once a client is connected to the one before.
pub fn create_private_pipe(name: &OsStr, first: bool) -> io::Result<NamedPipeServer> {
    // A protected DACL (nothing is inherited) with one entry: all access for the user, and nobody else.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", current_user_sid()?)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `sddl` is a NUL-terminated UTF-16 string that lives through the call, and `descriptor` is a place to write
    // to. The descriptor that is allocated is freed below, after the pipe has been made (the system keeps a copy).
    #[allow(unsafe_code)]
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` is a valid `SECURITY_ATTRIBUTES` whose descriptor is valid, and both live through the call.
    #[allow(unsafe_code)]
    let pipe = unsafe {
        options.create_with_security_attributes_raw(
            name,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
        )
    };
    // SAFETY: the descriptor came from the call above, and is freed once; the pipe has its own copy.
    #[allow(unsafe_code)]
    unsafe {
        LocalFree(descriptor.cast());
    }
    pipe
}

/// The pipe name for a user: `\\.\pipe\claude-ide-server-<sid>` (a pipe name is one name, with no further backslash).
pub fn default_pipe_name(sid: &str) -> String {
    format!(r"\\.\pipe\claude-ide-server-{sid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_a_process_that_cannot_exist_is_not() {
        assert!(process_is_alive(std::process::id()));
        // The largest id is not one that the system gives out.
        assert!(!process_is_alive(u32::MAX));
    }

    #[test]
    fn the_user_has_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[tokio::test]
    async fn a_pipe_is_for_one_server_at_a_time() {
        let name = format!(r"\\.\pipe\claude-ide-windows-test-{}", std::process::id());
        let first = create_private_pipe(OsStr::new(&name), true).unwrap();
        // The name is taken: another first instance is refused.
        let second = create_private_pipe(OsStr::new(&name), true).unwrap_err();
        assert_eq!(second.kind(), io::ErrorKind::PermissionDenied, "{second}");
        // An instance that is not the first is for waiting for the next client.
        let _waiting = create_private_pipe(OsStr::new(&name), false).unwrap();
        drop(first);
    }
}
