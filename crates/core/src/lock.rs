//! The lock files that Claude Code finds, and the record of them that lets a restarted server clean up.
//!
//! A lock file is `<port>.lock` in `~/.claude/ide/` (or `$CLAUDE_CONFIG_DIR/ide/`). The CLI lists that directory,
//! reads the JSON, and connects to the port. Nothing here reads the environment on its own: the directories and the
//! check that a process is alive are passed in, so that tests never touch the real `~/.claude`.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::create_private_dir;

/// The value of `transport` for a WebSocket.
pub const TRANSPORT_WS: &str = "ws";

/// The values of the environment that decide where the files go.
#[derive(Debug, Clone, Default)]
pub struct EnvVars {
    /// `CLAUDE_CONFIG_DIR`.
    pub claude_config_dir: Option<PathBuf>,
    /// `HOME`.
    pub home: Option<PathBuf>,
    /// `XDG_RUNTIME_DIR`.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// The numeric id of the user, for the fallback runtime directory.
    pub uid: u32,
}

impl EnvVars {
    /// Reads the real environment of this process. An empty variable counts as unset.
    pub fn from_process() -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            claude_config_dir: var("CLAUDE_CONFIG_DIR"),
            home: var("HOME"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
            uid: rustix::process::getuid().as_raw(),
        }
    }
}

/// The two directories this module works in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockDirs {
    /// Where the CLI looks for `<port>.lock`.
    pub lock_dir: PathBuf,
    /// Where the server records what it wrote, to clean up after a crash.
    pub record_dir: PathBuf,
}

impl LockDirs {
    /// The directories for the given environment: `$CLAUDE_CONFIG_DIR/ide`, else `~/.claude/ide`; and
    /// `$XDG_RUNTIME_DIR/claude-ide-server`, else a per-user directory under the temporary directory.
    pub fn from_env(env: &EnvVars) -> io::Result<Self> {
        let lock_dir = match (&env.claude_config_dir, &env.home) {
            (Some(config), _) => config.join("ide"),
            (None, Some(home)) => home.join(".claude").join("ide"),
            (None, None) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "neither CLAUDE_CONFIG_DIR nor HOME is set",
                ));
            }
        };
        let record_dir = match &env.xdg_runtime_dir {
            Some(runtime) => runtime.join("claude-ide-server"),
            None => std::env::temp_dir().join(format!("claude-ide-server-{}", env.uid)),
        };
        Ok(Self {
            lock_dir,
            record_dir,
        })
    }
}

/// What the CLI reads from a lock file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockContents {
    /// The process of the editor side (the adapter). The CLI deletes the file if it is not alive.
    pub pid: u32,
    /// The folders of the workspace, as the user would `cd` to them.
    pub workspace_folders: Vec<String>,
    /// The name shown in `/ide`.
    pub ide_name: String,
    /// [`TRANSPORT_WS`] for a WebSocket.
    pub transport: String,
    /// Whether the editor runs on Windows (relevant for WSL).
    pub running_in_windows: bool,
    /// The token that the CLI sends in the upgrade request.
    pub auth_token: String,
}

/// A 128-bit token from the operating system's random source, as 32 lowercase hexadecimal characters.
pub fn generate_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// What the server remembers about a lock file it wrote.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    lock_path: PathBuf,
    auth_token: String,
    server_pid: u32,
}

/// A lock file that this server wrote. Dropping it removes the file and its record.
#[derive(Debug)]
pub struct LockFile {
    path: PathBuf,
    record_path: PathBuf,
    contents: LockContents,
    server_pid: u32,
    closed: bool,
}

impl LockFile {
    /// Writes `<port>.lock` atomically (file mode 0600, directory 0700) and records it.
    ///
    /// `server_pid` is the process of this server, which decides whether a leftover record is still in use.
    pub fn create(
        dirs: &LockDirs,
        port: u16,
        contents: LockContents,
        server_pid: u32,
    ) -> io::Result<Self> {
        create_private_dir(&dirs.lock_dir)?;
        create_private_dir(&dirs.record_dir)?;
        let path = dirs.lock_dir.join(format!("{port}.lock"));
        let record_path = dirs.record_dir.join(format!("{port}.json"));

        // The record comes first: a crash between the two leaves a record without a lock file, which the cleanup
        // handles, and never a lock file that nothing remembers.
        let record = Record {
            lock_path: path.clone(),
            auth_token: contents.auth_token.clone(),
            server_pid,
        };
        write_atomically(&record_path, &to_json(&record)?, server_pid)?;
        if let Err(error) = write_atomically(&path, &to_json(&contents)?, server_pid) {
            let _ = fs::remove_file(&record_path);
            return Err(error);
        }
        Ok(Self {
            path,
            record_path,
            contents,
            server_pid,
            closed: false,
        })
    }

    /// The path of the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the file holds now.
    pub fn contents(&self) -> &LockContents {
        &self.contents
    }

    /// Rewrites the file (atomically) with new `workspaceFolders`.
    pub fn set_workspace_folders(&mut self, folders: Vec<String>) -> io::Result<()> {
        let mut updated = self.contents.clone();
        updated.workspace_folders = folders;
        write_atomically(&self.path, &to_json(&updated)?, self.server_pid)?;
        self.contents = updated;
        Ok(())
    }

    /// Removes the lock file and its record. Safe to call twice.
    pub fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let lock = remove_if_exists(&self.path);
        let record = remove_if_exists(&self.record_path);
        lock.and(record)
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Removes the lock files that a server which is no longer alive left behind, and their records. Returns the lock
/// files that were removed.
///
/// A lock file is removed only if it still holds the token that was recorded, so a file that something else wrote
/// under the same name is left alone. A record whose server is still alive is left alone.
pub fn cleanup_stale(dirs: &LockDirs, is_alive: impl Fn(u32) -> bool) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(&dirs.record_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut removed = Vec::new();
    for entry in entries {
        let record_path = entry?.path();
        if record_path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let record: Option<Record> = fs::read(&record_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let Some(record) = record else {
            // Unreadable: nothing can be cleaned up from it.
            let _ = fs::remove_file(&record_path);
            continue;
        };
        if is_alive(record.server_pid) {
            continue;
        }
        let holds_our_token = fs::read(&record.lock_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LockContents>(&bytes).ok())
            .is_some_and(|lock| lock.auth_token == record.auth_token);
        if holds_our_token && fs::remove_file(&record.lock_path).is_ok() {
            removed.push(record.lock_path.clone());
        }
        let _ = fs::remove_file(&record_path);
    }
    removed.sort();
    Ok(removed)
}

/// Whether a process with this id exists on this machine.
pub fn process_is_alive(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        // The process exists, but belongs to someone else.
        Err(error) => error == rustix::io::Errno::PERM,
    }
}

fn to_json<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(io::Error::other)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Writes to a temporary name in the same directory, then renames. A reader sees the old file or the new one, never
/// half of one. The temporary name does not end in `.lock`, so the CLI does not list it.
fn write_atomically(path: &Path, bytes: &[u8], server_pid: u32) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temporary = path.with_file_name(format!(".{name}.{server_pid}.tmp"));
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}
