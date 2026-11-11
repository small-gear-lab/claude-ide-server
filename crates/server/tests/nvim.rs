//! The Neovim adapter (`editors/nvim`) in a headless Neovim, against the real executable.
//!
//! These need `nvim` on the path. Without it they say so and pass, unless `CLAUDE_IDE_REQUIRE_NVIM` is set (CI sets it),
//! so that a machine without Neovim can still run the other tests and a CI run cannot pass by skipping these.

// The stand-in for Claude Code is the core's own.
#[path = "../../core/tests/common/mod.rs"]
mod common;
mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use claude_ide_core::lock::LockContents;
use common::claude::MockClaude;
use serde_json::Value;
use support::{Home, Server, WAIT, connect_when_ready};
use tokio::time::timeout;

/// A headless Neovim with the adapter on its runtime path, killed when dropped.
struct Nvim(Child);

impl Nvim {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Nvim {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Whether Neovim can be run; says why not, and fails if it is required.
fn nvim_available() -> bool {
    let found = Command::new("nvim")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !found {
        assert!(
            std::env::var_os("CLAUDE_IDE_REQUIRE_NVIM").is_none(),
            "nvim is required (CLAUDE_IDE_REQUIRE_NVIM is set) and was not found"
        );
        eprintln!("skipped: nvim is not on the path");
    }
    found
}

fn adapter_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editors/nvim")
}

/// Starts Neovim, which runs `lua` after the adapter is on its runtime path. Nothing of the user's own is read or
/// written: no configuration, no shada, and the directories of the XDG variables are under the test's directory.
fn start_nvim(home: &Home, lua: &str) -> Nvim {
    start_nvim_listening(home, lua, None)
}

/// As [`start_nvim`], and listening on `listen`, so that keys can be sent to it (`nvim --server <listen> --remote-send`).
fn start_nvim_listening(home: &Home, lua: &str, listen: Option<&Path>) -> Nvim {
    let root = home.dir.path();
    let mut command = Command::new("nvim");
    command.args(["--headless", "-u", "NONE", "-i", "NONE"]);
    if let Some(listen) = listen {
        command.arg("--listen").arg(listen);
    }
    let child = command
        .arg("--cmd")
        .arg(format!("set runtimepath^={}", adapter_dir().display()))
        .arg("-c")
        .arg(format!("lua {lua}"))
        // What a server that the adapter starts inherits: it must not touch the user's own lock files either.
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("XDG_RUNTIME_DIR", root.join("xdg"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("HOME", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Nvim(child)
}

#[test]
fn the_checks_that_need_no_server_pass() {
    if !nvim_available() {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("nvim")
        .args([
            "--headless",
            "-u",
            "NONE",
            "-i",
            "NONE",
            "-l",
            "editors/nvim/tests/unit.lua",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The adapter with a short wait between attempts, so that the tests do not wait for the usual one.
const SETUP: &str = "require('claude_ide').setup({ reconnect = { initial_ms = 50, max_ms = 200 }, server = { autostart = false } })";

fn start_server(home: &Home) -> Server {
    Server(home.command().spawn().unwrap())
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    timeout(WAIT, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

fn read_lock(home: &Home) -> LockContents {
    let files = home.lock_files();
    assert_eq!(files.len(), 1, "one lock file: {files:?}");
    serde_json::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap()
}

#[tokio::test]
async fn it_registers_at_the_default_socket_and_goes_away_with_the_process() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    // No `--socket`: the adapter and the server agree where the socket is.
    let _ = connect_when_ready(&home.default_socket()).await;

    let mut nvim = start_nvim(&home, SETUP);
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let lock = read_lock(&home);
    assert_eq!(lock.ide_name, "Neovim");
    // The process of the lock file is this Neovim: the CLI deletes a lock file whose process is gone.
    assert_eq!(lock.pid, nvim.pid());

    nvim.kill();
    wait_until("the lock file goes with the process", || {
        home.lock_files().is_empty()
    })
    .await;
}

#[tokio::test]
async fn a_neovim_that_starts_before_the_server_connects_when_the_server_comes() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let nvim = start_nvim(&home, SETUP);
    // It tries, and keeps quiet, while there is no server.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(home.lock_files().is_empty());

    let _server = start_server(&home);
    wait_until("the adapter registers once the server is there", || {
        !home.lock_files().is_empty()
    })
    .await;
    assert_eq!(read_lock(&home).pid, nvim.pid());
}

#[tokio::test]
async fn it_connects_again_when_the_server_restarts() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let mut server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;
    let nvim = start_nvim(&home, SETUP);
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;

    // The server is told to stop: it tells the adapter, and removes the lock file.
    server.terminate();
    server.wait().await;
    assert!(home.lock_files().is_empty());

    let _second = start_server(&home);
    wait_until("the adapter registers with the server that follows", || {
        !home.lock_files().is_empty()
    })
    .await;
    assert_eq!(read_lock(&home).pid, nvim.pid());
}

#[tokio::test]
async fn a_neovim_that_is_not_to_register_stays_away() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    // A Neovim that is not to register (a child window, in the author's setup) does not, however long it runs.
    let _quiet = start_nvim(
        &home,
        "require('claude_ide').setup({ should_register = function() return false end, server = { autostart = false } })",
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(home.lock_files().is_empty());
}

#[tokio::test]
async fn the_workspace_follows_cd() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    // Moved by `:cd` once it is registered.
    let folder = home.dir.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    let _nvim = start_nvim(
        &home,
        &format!(
            "require('claude_ide').setup({{ server = {{ autostart = false }}, on_registered = function() vim.cmd('cd {}') end }})",
            folder.display()
        ),
    );
    let folder = std::fs::canonicalize(&folder).unwrap();
    wait_until("the lock file names the folder", || {
        home.lock_files().len() == 1
            && read_lock(&home).workspace_folders == [folder.to_string_lossy()]
    })
    .await;
}

/// The next notification of `method` that Claude Code gets.
async fn next_notification(claude: &mut MockClaude, method: &str) -> Value {
    timeout(WAIT, async {
        loop {
            let message = claude.recv_json().await.expect("the connection stays open");
            if message["method"] == method {
                return message["params"].clone();
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("a {method} arrives"))
}

#[tokio::test]
async fn the_selection_reaches_claude_code_when_it_connects_and_when_the_cursor_moves() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let file = home.dir.path().join("a.txt");
    std::fs::write(&file, "aé😀b\nsecond line\n").unwrap();
    let listen = home.dir.path().join("nvim.sock");
    let _nvim = start_nvim_listening(
        &home,
        &format!(
            "vim.cmd('edit {}'); require('claude_ide').setup({{ selection_debounce_ms = 20, server = {{ autostart = false }} }})",
            file.display()
        ),
        Some(&listen),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let lock = read_lock(&home);
    let port: u16 = home.lock_files()[0]
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .parse()
        .unwrap();

    // A Claude Code that connects now starts from the selection that there is: the cursor, at the start of the file.
    let mut claude = MockClaude::connect(port, &lock.auth_token).await;
    claude.handshake(4242).await;
    let first = next_notification(&mut claude, "selection_changed").await;
    // The real path of the file: Neovim names a file by it, and on macOS the temporary directory is under a link
    // (`/var` is `/private/var`).
    let real = std::fs::canonicalize(&file).unwrap();
    assert_eq!(first["filePath"], real.to_string_lossy().as_ref());
    assert_eq!(first["selection"]["isEmpty"], true);
    assert_eq!(first["selection"]["start"]["character"], 0);

    // Two characters to the right: past `a` and `é`, onto the emoji. The column is counted in UTF-16 code units, not
    // in bytes (the emoji starts at byte 3).
    wait_until("nvim listens", || listen.exists()).await;
    let sent = Command::new("nvim")
        .arg("--server")
        .arg(&listen)
        .args(["--remote-send", "2l"])
        .status()
        .unwrap();
    assert!(sent.success());
    let moved = next_notification(&mut claude, "selection_changed").await;
    assert_eq!(moved["selection"]["start"]["line"], 0);
    assert_eq!(moved["selection"]["start"]["character"], 2);
}

/// The adapter set to start the real executable, which ends one second after the last adapter has gone.
fn starts_the_server() -> String {
    format!(
        "require('claude_ide').setup({{ reconnect = {{ initial_ms = 50, max_ms = 200 }}, \
         server = {{ command = '{}', args = {{ '--idle-timeout', '1' }} }} }})",
        env!("CARGO_BIN_EXE_claude-ide-server")
    )
}

async fn nobody_listens(home: &Home) -> bool {
    tokio::net::UnixStream::connect(home.default_socket())
        .await
        .is_err()
}

#[tokio::test]
async fn a_neovim_starts_the_server_when_nobody_is_listening_and_it_ends_when_the_neovim_is_gone() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    assert!(nobody_listens(&home).await);

    let mut nvim = start_nvim(&home, &starts_the_server());
    wait_until(
        "the adapter registers with the server that it started",
        || !home.lock_files().is_empty(),
    )
    .await;
    assert_eq!(read_lock(&home).pid, nvim.pid());

    // The server was not a child of Neovim and does not end with it: it ends by itself, when it has been idle.
    nvim.kill();
    let mut ended = false;
    for _ in 0..250 {
        if nobody_listens(&home).await {
            ended = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ended, "the server ends once it has been idle");
    assert!(home.lock_files().is_empty());
}

#[tokio::test]
async fn neovims_that_start_together_share_one_server() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let first = start_nvim(&home, &starts_the_server());
    let second = start_nvim(&home, &starts_the_server());
    wait_until("both register", || home.lock_files().len() == 2).await;
    let pids: Vec<u32> = home
        .lock_files()
        .iter()
        .map(|path| {
            let contents: LockContents =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            contents.pid
        })
        .collect();
    assert!(pids.contains(&first.pid()) && pids.contains(&second.pid()));
}

#[tokio::test]
async fn a_server_that_the_adapter_started_and_that_does_not_understand_its_arguments_is_reported()
{
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let messages = home.dir.path().join("messages.txt");
    // The server's own output goes nowhere, so the exit code is what the user hears.
    let nvim = start_nvim(
        &home,
        &format!(
            "vim.notify = function(message) local file = io.open('{}', 'a'); file:write(message, '\\n'); file:close() end; \
             require('claude_ide').setup({{ reconnect = {{ initial_ms = 50, max_ms = 200 }}, \
             server = {{ command = '{}', args = {{ '--no-such-switch' }} }} }})",
            messages.display(),
            env!("CARGO_BIN_EXE_claude-ide-server")
        ),
    );
    wait_until("the user is told", || {
        std::fs::read_to_string(&messages).is_ok_and(|text| text.contains("exited with code 2"))
    })
    .await;
    assert!(home.lock_files().is_empty());
    drop(nvim);
}
