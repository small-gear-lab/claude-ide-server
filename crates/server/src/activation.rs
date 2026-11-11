//! Where the adapter socket comes from.
//!
//! The server does not care how it was started: by hand, by a supervisor that listens for it and starts it at the first
//! connection (socket activation), or by an editor adapter that found no server. What differs is how the listening
//! socket is obtained, and that is the one thing this module hides. Each way is a [`SocketSource`]; the first that
//! applies is used, and the rest of the server sees only the listener.
//!
//! | source | for | how |
//! |---|---|---|
//! | [`Handed`] | systemd, and other supervisors and container runtimes that follow the same convention | the socket is already listening on a file descriptor that the process inherits (`LISTEN_FDS`) |
//! | [`Bind`] | everything else, and the default | the server makes the socket itself |
//!
//! What a system does differently is added here, in a source of its own and selected by `cfg` where the system decides
//! it (launchd on macOS, which hands a socket over by name through its own interface; Task Scheduler on Windows, which
//! has no socket activation and needs a different kind of endpoint). Until a system has its own source it uses
//! [`Bind`], with the supervisor of the system starting the executable at log-in.

use std::io;
use std::path::PathBuf;

use claude_ide_core::adapter::UnixListener;

/// One way of getting the adapter socket.
pub trait SocketSource {
    /// What to call it in the log.
    fn name(&self) -> &'static str;

    /// The listener, or `None` if this source does not apply here (the process was not started that way). An error
    /// means that it applies and failed: the next source is not tried, since the supervisor expects its socket to be
    /// the one that is served.
    fn open(&self) -> io::Result<Option<UnixListener>>;
}

/// The first source that applies, with its name.
pub fn open_first(sources: &[Box<dyn SocketSource>]) -> io::Result<(UnixListener, &'static str)> {
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

/// The sources for this system, in the order they are tried: a socket that was handed over, then making our own.
pub fn default_sources(path: PathBuf) -> Vec<Box<dyn SocketSource>> {
    vec![Box::new(Handed), Box::new(Bind { path })]
}

/// A socket that is already listening, inherited from the process that started the server (`LISTEN_PID` is this
/// process and `LISTEN_FDS` says how many; the first is used). It is not removed when the server exits.
pub struct Handed;

impl SocketSource for Handed {
    fn name(&self) -> &'static str {
        "handed over (LISTEN_FDS)"
    }

    fn open(&self) -> io::Result<Option<UnixListener>> {
        let mut fds = listenfd::ListenFd::from_env();
        match fds.take_unix_listener(0)? {
            Some(socket) => UnixListener::from_listening(socket).map(Some),
            None => Ok(None),
        }
    }
}

/// The server makes the socket itself, at `path`.
pub struct Bind {
    pub path: PathBuf,
}

impl SocketSource for Bind {
    fn name(&self) -> &'static str {
        "bound by the server"
    }

    fn open(&self) -> io::Result<Option<UnixListener>> {
        UnixListener::bind(&self.path).map(Some)
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
        fn open(&self) -> io::Result<Option<UnixListener>> {
            Ok(None)
        }
    }

    struct Broken;
    impl SocketSource for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn open(&self) -> io::Result<Option<UnixListener>> {
            Err(io::Error::other("it applies, and fails"))
        }
    }

    #[tokio::test]
    async fn the_first_source_that_applies_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sock");
        let sources: Vec<Box<dyn SocketSource>> =
            vec![Box::new(Never), Box::new(Bind { path: path.clone() })];
        let (listener, name) = open_first(&sources).unwrap();
        assert_eq!(name, "bound by the server");
        assert_eq!(listener.path(), Some(path.as_path()));
    }

    #[tokio::test]
    async fn a_source_that_applies_and_fails_ends_the_search() {
        let dir = tempfile::tempdir().unwrap();
        let sources: Vec<Box<dyn SocketSource>> = vec![
            Box::new(Broken),
            Box::new(Bind {
                path: dir.path().join("a.sock"),
            }),
        ];
        assert!(open_first(&sources).is_err());
        assert!(!dir.path().join("a.sock").exists());
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
