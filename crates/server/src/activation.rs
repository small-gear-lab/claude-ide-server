//! Where the adapter socket comes from.
//!
//! The server does not care how it was started: by hand, by a supervisor that listens for it and starts it at the first
//! connection (socket activation), or by an editor adapter that found no server. What differs is how the listening
//! socket is obtained, and that is the one thing this module hides. Each way is a [`SocketSource`]; the first that
//! applies is used, and the rest of the server sees only the listener.
//!
//! | source | for | how |
//! |---|---|---|
//! | `Handed` (Unix) | systemd, and other supervisors and container runtimes that follow the same convention | the socket is already listening on a file descriptor that the process inherits (`LISTEN_FDS`) |
//! | `Launchd` (macOS) | launchd | the socket is asked for by name, from the property list of the job (`launch_activate_socket`) |
//! | [`Bind`] | everything else, and the default | the server makes the socket itself (a named pipe on Windows) |
//!
//! What a system does differently is added here, in a source of its own and selected by `cfg` where the system decides
//! it (launchd on macOS, which hands a socket over by name through its own interface; Task Scheduler on Windows, which
//! has no socket activation and needs a different kind of endpoint). Until a system has its own source it uses
//! [`Bind`], with the supervisor of the system starting the executable at log-in.
//!
//! The call that launchd's source needs is C, and is wrapped in a crate of its own (`claude-ide-launchd`), so that this
//! crate stays free of `unsafe`.

use std::io;
use std::path::PathBuf;

use claude_ide_core::adapter::NativeListener;
#[cfg(unix)]
use claude_ide_core::adapter::UnixListener;

/// One way of getting the adapter socket.
pub trait SocketSource {
    /// What to call it in the log.
    fn name(&self) -> &'static str;

    /// The listener, or `None` if this source does not apply here (the process was not started that way). An error
    /// means that it applies and failed: the next source is not tried, since the supervisor expects its socket to be
    /// the one that is served.
    fn open(&self) -> io::Result<Option<NativeListener>>;
}

/// The first source that applies, with its name.
pub fn open_first(sources: &[Box<dyn SocketSource>]) -> io::Result<(NativeListener, &'static str)> {
    for source in sources {
        if let Some(listener) = source.open()? {
            return Ok((listener, source.name()));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no way of getting the adapter socket applies",
    ))
}

/// The name of the socket in the `Sockets` dictionary of the property list for launchd (`contrib/launchd`).
#[cfg(target_os = "macos")]
pub const LAUNCHD_SOCKET_NAME: &str = "adapter";

/// The sources for this system, in the order they are tried: a socket that was handed over (by the convention of
/// systemd, and, on macOS, by launchd), then making our own.
pub fn default_sources(path: PathBuf) -> Vec<Box<dyn SocketSource>> {
    vec![
        #[cfg(unix)]
        Box::new(Handed),
        #[cfg(target_os = "macos")]
        Box::new(Launchd {
            name: LAUNCHD_SOCKET_NAME,
        }),
        Box::new(Bind { path }),
    ]
}

/// A socket that is already listening, inherited from the process that started the server (`LISTEN_PID` is this
/// process and `LISTEN_FDS` says how many; the first is used). It is not removed when the server exits.
#[cfg(unix)]
pub struct Handed;

#[cfg(unix)]
impl SocketSource for Handed {
    fn name(&self) -> &'static str {
        "handed over (LISTEN_FDS)"
    }

    fn open(&self) -> io::Result<Option<NativeListener>> {
        let mut fds = listenfd::ListenFd::from_env();
        match fds.take_unix_listener(0)? {
            Some(socket) => UnixListener::from_listening(socket).map(Some),
            None => Ok(None),
        }
    }
}

/// A socket that launchd listens on, asked for by the name that it has in the `Sockets` dictionary of the property list of
/// the job. It does not apply when launchd did not start this process (the server was run by hand, say), or started it
/// from a property list that has no such socket (the one that only runs the server at log-in): the server then makes its own.
/// Like a handed-over socket it is not removed when the server exits, and launchd starts the server again at the next
/// connection.
#[cfg(target_os = "macos")]
pub struct Launchd {
    pub name: &'static str,
}

#[cfg(target_os = "macos")]
impl SocketSource for Launchd {
    fn name(&self) -> &'static str {
        "handed over by launchd"
    }

    fn open(&self) -> io::Result<Option<NativeListener>> {
        use claude_ide_launchd::{Error, activate_socket};
        match activate_socket(self.name) {
            Ok(fds) => {
                // One socket: the first is used, and the others (there are none for a socket of one path) are closed.
                let Some(fd) = fds.into_iter().next() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "launchd handed over no socket",
                    ));
                };
                UnixListener::from_listening(std::os::unix::net::UnixListener::from(fd)).map(Some)
            }
            Err(Error::NotManaged | Error::NotInPlist) => Ok(None),
            Err(Error::Failed(error)) => Err(error),
        }
    }
}

/// The server makes the socket itself, at `path` (on Windows, the named pipe of that name).
pub struct Bind {
    pub path: PathBuf,
}

impl SocketSource for Bind {
    fn name(&self) -> &'static str {
        "bound by the server"
    }

    fn open(&self) -> io::Result<Option<NativeListener>> {
        NativeListener::bind(&self.path).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Never;
    impl SocketSource for Never {
        fn name(&self) -> &'static str {
            "never"
        }
        fn open(&self) -> io::Result<Option<NativeListener>> {
            Ok(None)
        }
    }

    struct Broken;
    impl SocketSource for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn open(&self) -> io::Result<Option<NativeListener>> {
            Err(io::Error::other("it applies, and fails"))
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_launchd_source_does_not_apply_to_a_process_that_launchd_did_not_start() {
        // `Launchd` asks for a name that no property list has: it says that it does not apply, and does not fail.
        let source = Launchd {
            name: "claude-ide-server-test",
        };
        assert!(matches!(source.open(), Ok(None)));
    }

    /// Where a test binds: a socket file in the directory (Unix), or a pipe with a name of its own (Windows, which has no
    /// directory for it).
    fn endpoint(dir: &tempfile::TempDir, test: &str) -> PathBuf {
        #[cfg(unix)]
        {
            let _ = test;
            dir.path().join("a.sock")
        }
        #[cfg(windows)]
        {
            let _ = dir;
            PathBuf::from(format!(
                r"\\.\pipe\claude-ide-server-test-{}-{test}",
                std::process::id()
            ))
        }
    }

    #[tokio::test]
    async fn the_first_source_that_applies_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = endpoint(&dir, "first");
        let sources: Vec<Box<dyn SocketSource>> =
            vec![Box::new(Never), Box::new(Bind { path: path.clone() })];
        let (listener, name) = open_first(&sources).unwrap();
        assert_eq!(name, "bound by the server");
        assert_eq!(listener.path(), Some(path.as_path()));
    }

    #[tokio::test]
    async fn a_source_that_applies_and_fails_ends_the_search() {
        let dir = tempfile::tempdir().unwrap();
        let path = endpoint(&dir, "broken");
        let sources: Vec<Box<dyn SocketSource>> =
            vec![Box::new(Broken), Box::new(Bind { path: path.clone() })];
        assert!(open_first(&sources).is_err());
        // The source after it was not tried: nothing was made at the place that it would have bound.
        #[cfg(unix)]
        assert!(!path.exists());
        #[cfg(windows)]
        assert!(claude_ide_core::adapter::PipeListener::bind(&path).is_ok());
    }

    #[test]
    fn with_no_source_that_applies_it_is_an_error() {
        let sources: Vec<Box<dyn SocketSource>> = vec![Box::new(Never)];
        assert_eq!(
            open_first(&sources).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
