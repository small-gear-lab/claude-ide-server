//! What the tests of the executable share: a directory to work in, and a server that is a child process.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use claude_ide_core::lock::LockContents;
use tempfile::TempDir;
use tokio::time::timeout;

pub const WAIT: Duration = Duration::from_secs(10);

/// The directories that a server started by a test works in: all of them under one temporary directory.
pub struct Home {
    pub dir: TempDir,
}

impl Home {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    /// Where the test server is told to listen: a socket file in the directory (Unix), or a named pipe whose name has the
    /// name of the directory in it (Windows, where a pipe has no directory, and the name must be one that no other test has).
    pub fn socket(&self) -> PathBuf {
        #[cfg(unix)]
        {
            self.dir.path().join("run").join("adapter.sock")
        }
        #[cfg(windows)]
        {
            let unique = self.dir.path().file_name().unwrap().to_string_lossy();
            PathBuf::from(format!(r"\\.\pipe\claude-ide-test-{unique}"))
        }
    }

    /// Where a server that is not given `--socket` puts the socket, with the environment of `command`: under the
    /// runtime directory on Unix, which tests the default; on Windows the pipe of this test, which the environment says
    /// (`CLAUDE_IDE_SERVER_SOCKET`; the default there is the pipe of the user, which every test would share).
    pub fn default_socket(&self) -> PathBuf {
        #[cfg(unix)]
        {
            self.dir
                .path()
                .join("xdg")
                .join("claude-ide-server")
                .join("adapter.sock")
        }
        #[cfg(windows)]
        {
            self.socket()
        }
    }

    /// What a test that talks to a Neovim listens on: a socket file in the directory, or a pipe of this test's own.
    pub fn nvim_listen(&self) -> PathBuf {
        #[cfg(unix)]
        {
            self.dir.path().join("nvim.sock")
        }
        #[cfg(windows)]
        {
            let unique = self.dir.path().file_name().unwrap().to_string_lossy();
            PathBuf::from(format!(r"\\.\pipe\claude-ide-nvim-test-{unique}"))
        }
    }

    pub fn lock_dir(&self) -> PathBuf {
        self.dir.path().join("claude").join("ide")
    }

    /// The server, with its environment pointed at this directory and its log in a file.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_claude-ide-server"));
        command
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("claude"))
            .env("XDG_RUNTIME_DIR", self.dir.path().join("xdg"))
            .env("HOME", self.dir.path())
            .env("USERPROFILE", self.dir.path())
            .env_remove("CLAUDE_IDE_SERVER_SOCKET")
            .env_remove("LISTEN_FDS")
            .env_remove("LISTEN_PID")
            .env_remove("RUST_LOG")
            .env_remove("CLAUDE_IDE_SERVER_LOG")
            .stdin(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(self.log_path()).unwrap()));
        #[cfg(windows)]
        command.env("CLAUDE_IDE_SERVER_SOCKET", self.socket());
        command
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.path().join("server.log")
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    pub fn lock_files(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.lock_dir()) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "lock"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// A child process that is killed if a test fails before it is waited for.
pub struct Server(pub Child);

impl Server {
    /// Asks the server to end, as a supervisor does (`SIGTERM`).
    #[cfg(unix)]
    pub fn terminate(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.0.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Ends the server at once, with no chance to clean up (what a process gets when Windows ends a task, or `kill -9`).
    pub fn kill(&mut self) {
        self.0.kill().unwrap();
    }

    /// Tells the server to stop, as whoever runs it would: `SIGTERM` on Unix, and on Windows, which has no such signal for
    /// a program with no window, the end that a task gets (the lock file goes all the same, with the process).
    pub fn stop(&mut self) {
        #[cfg(unix)]
        self.terminate();
        #[cfg(windows)]
        self.kill();
    }

    pub async fn wait(&mut self) -> ExitStatus {
        timeout(WAIT, async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the server ends")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub async fn connect_when_ready(socket: &Path) -> crate::common::AdapterStream {
    timeout(WAIT, async {
        loop {
            if let Ok(stream) = crate::common::connect_to_adapters(socket).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the socket is there")
}

pub async fn wait_for_log(home: &Home, text: &str) {
    timeout(WAIT, async {
        while !home.log().contains(text) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the log says {text:?}:\n{}", home.log()));
}

pub fn token_of(lock_files: &[PathBuf], port: u16) -> String {
    let path = lock_files
        .iter()
        .find(|path| {
            path.file_stem()
                .is_some_and(|stem| stem.to_string_lossy() == port.to_string())
        })
        .expect("the lock file of the port");
    let contents: LockContents =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    contents.auth_token
}
