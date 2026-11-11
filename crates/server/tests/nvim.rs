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
    start_nvim_in(home, home.dir.path(), lua, listen)
}

/// As [`start_nvim_listening`], with `cwd` as its working directory (so its workspace).
fn start_nvim_in(home: &Home, cwd: &Path, lua: &str, listen: Option<&Path>) -> Nvim {
    let root = home.dir.path();
    let mut command = Command::new("nvim");
    // `-u NORC` and not `-u NONE`: no configuration is read, but the plugins on the runtime path are loaded, as they
    // are for a user (the adapter's commands are in its `plugin` directory).
    command.args(["--headless", "-u", "NORC", "-i", "NONE"]);
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
        .current_dir(cwd)
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

/// Waits until the server that an adapter started has ended: it ends by itself, when no adapter has been there for the idle
/// time, so that nothing of the test is left running (or doing something with a directory that is about to be removed).
async fn wait_for_the_server_to_end(home: &Home) {
    let mut ended = false;
    for _ in 0..250 {
        if nobody_listens(home).await {
            ended = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ended, "the server ends once it has been idle");
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
    wait_for_the_server_to_end(&home).await;
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
    // One server was started, and nothing is left of it when the test is over.
    drop((first, second));
    wait_for_the_server_to_end(&home).await;
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

/// A Claude Code that is connected to the workspace that a Neovim has registered.
async fn claude_for(home: &Home) -> MockClaude {
    let lock = read_lock(home);
    let port: u16 = home.lock_files()[0]
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .parse()
        .unwrap();
    let mut claude = MockClaude::connect(port, &lock.auth_token).await;
    claude.handshake(4242).await;
    claude
}

/// `tools/call`, and the JSON in the text of the result. The server has checked it against its own types, so a shape
/// that it does not accept is an error here, with the place of the fault.
async fn call(claude: &mut MockClaude, name: &str) -> Value {
    call_with(claude, name, serde_json::json!({})).await
}

/// `tools/call`, and the text of the result as it is (for the results that are a message and not JSON).
async fn call_text(claude: &mut MockClaude, name: &str, arguments: Value) -> String {
    let answer = claude
        .request(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await;
    assert_ne!(answer["result"]["isError"], true, "{name}: {answer}");
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {answer}"))
        .to_owned()
}

async fn call_with(claude: &mut MockClaude, name: &str, arguments: Value) -> Value {
    let answer = claude
        .request(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await;
    assert_ne!(answer["result"]["isError"], true, "{name}: {answer}");
    common::scene::result_json(&answer)
}

#[tokio::test]
async fn claude_code_can_ask_for_the_open_editors_and_the_selection() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let first = home.dir.path().join("a.txt");
    let second = home.dir.path().join("b.lua");
    std::fs::write(&first, "aé😀b\nsecond line\n").unwrap();
    std::fs::write(&second, "return 1\n").unwrap();
    let _nvim = start_nvim(
        &home,
        &format!(
            "vim.cmd('edit {}'); vim.cmd('badd {}'); \
             require('claude_ide').setup({{ selection_debounce_ms = 20, server = {{ autostart = false }} }})",
            first.display(),
            second.display()
        ),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let mut claude = claude_for(&home).await;

    // The tools that the adapter can answer are offered.
    let tools = claude.tool_names().await;
    for name in [
        "getOpenEditors",
        "getCurrentSelection",
        "getLatestSelection",
    ] {
        assert!(
            tools.contains(&name.to_owned()),
            "{name} is offered: {tools:?}"
        );
    }

    // Both files are open, and the first is the active one.
    let editors = call(&mut claude, "getOpenEditors").await;
    let tabs = editors["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2, "{editors}");
    let real_first = std::fs::canonicalize(&first).unwrap();
    let active: Vec<_> = tabs.iter().filter(|tab| tab["isActive"] == true).collect();
    assert_eq!(active.len(), 1, "{editors}");
    assert_eq!(active[0]["label"], "a.txt");
    assert_eq!(active[0]["uri"], format!("file://{}", real_first.display()));
    assert_eq!(active[0]["languageId"], "plaintext");
    assert_eq!(active[0]["isDirty"], false);
    // What the extension adds to a tab: where it is shown, and what the document is.
    assert_eq!(active[0]["fileName"], real_first.to_string_lossy().as_ref());
    assert_eq!(
        active[0]["lineCount"], 3,
        "two lines and the empty one after the final newline"
    );
    assert_eq!(active[0]["isUntitled"], false);
    assert_eq!(active[0]["isPinned"], false);
    assert_eq!(active[0]["isPreview"], false);
    assert_eq!(active[0]["groupIndex"], 0);
    assert_eq!(active[0]["viewColumn"], 1);
    assert_eq!(active[0]["isGroupActive"], true);
    assert_eq!(active[0]["selection"]["isReversed"], false);
    let other = tabs.iter().find(|tab| tab["isActive"] == false).unwrap();
    // A file that no window shows has no place.
    assert!(other.get("groupIndex").is_none(), "{other}");
    assert_eq!(other["label"], "b.lua");
    assert_eq!(other["languageId"], "lua");

    // The selection is the cursor at the start of the file; the latest one is the same.
    for name in ["getCurrentSelection", "getLatestSelection"] {
        let selection = call(&mut claude, name).await;
        // The current selection says `success`, and the latest one does not (as in the VS Code extension).
        if name == "getCurrentSelection" {
            assert_eq!(selection["success"], true, "{selection}");
        } else {
            assert!(selection.get("success").is_none(), "{selection}");
        }
        assert_eq!(selection["filePath"], real_first.to_string_lossy().as_ref());
        assert_eq!(selection["selection"]["isEmpty"], true);
        assert_eq!(selection["selection"]["start"]["line"], 0);
    }
}

#[tokio::test]
async fn claude_code_can_ask_for_the_diagnostics_of_a_file_and_of_all() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let first = home.dir.path().join("a.txt");
    let second = home.dir.path().join("b.lua");
    std::fs::write(&first, "aé😀b\nsecond line\n").unwrap();
    std::fs::write(&second, "return 1\n").unwrap();
    // One diagnostic in the first file, on the emoji: bytes 3 to 7 of the first line, which are the columns 2 to 4 in
    // UTF-16 code units.
    let _nvim = start_nvim(
        &home,
        &format!(
            "vim.cmd('edit {}'); vim.cmd('badd {}'); \
             vim.diagnostic.set(vim.api.nvim_create_namespace('test'), 0, {{ {{ lnum = 0, col = 3, end_lnum = 0, end_col = 7, \
             severity = vim.diagnostic.severity.ERROR, message = 'boom', source = 'lint' }} }}); \
             require('claude_ide').setup({{ server = {{ autostart = false }} }})",
            first.display(),
            second.display()
        ),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let mut claude = claude_for(&home).await;
    let real_first = std::fs::canonicalize(&first).unwrap();
    let first_uri = format!("file://{}", real_first.display());

    // Of all the files: only the one that has some.
    let all = call(&mut claude, "getDiagnostics").await;
    let files = all.as_array().unwrap();
    assert_eq!(files.len(), 1, "{all}");
    assert_eq!(files[0]["uri"], first_uri.as_str());
    let diagnostic = &files[0]["diagnostics"][0];
    assert_eq!(diagnostic["message"], "boom");
    assert_eq!(diagnostic["severity"], "Error");
    assert_eq!(diagnostic["source"], "lint");
    assert_eq!(diagnostic["range"]["start"]["character"], 2);
    assert_eq!(diagnostic["range"]["end"]["character"], 4);

    // Of one file: the same, and for a file that has none, an empty list; for a file that is not open, nothing.
    let one = call_with(
        &mut claude,
        "getDiagnostics",
        serde_json::json!({ "uri": first_uri }),
    )
    .await;
    assert_eq!(one, all);
    let none = call_with(
        &mut claude,
        "getDiagnostics",
        serde_json::json!({ "uri": format!("file://{}", std::fs::canonicalize(&second).unwrap().display()) }),
    )
    .await;
    assert_eq!(none[0]["diagnostics"], serde_json::json!([]));
    let unknown = call_with(
        &mut claude,
        "getDiagnostics",
        serde_json::json!({ "uri": "file:///nowhere/at/all.txt" }),
    )
    .await;
    assert_eq!(unknown, serde_json::json!([]));
}

/// Types `keys` into a Neovim that listens on `listen`.
fn send_keys(listen: &Path, keys: &str) {
    let sent = Command::new("nvim")
        .arg("--server")
        .arg(listen)
        .args(["--remote-send", keys])
        .status()
        .unwrap();
    assert!(sent.success());
}

#[tokio::test]
async fn a_file_or_some_of_its_lines_can_be_mentioned_to_claude_code() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let file = home.dir.path().join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();
    let listen = home.dir.path().join("nvim.sock");
    let _nvim = start_nvim_listening(
        &home,
        &format!(
            "vim.cmd('edit {}'); require('claude_ide').setup({{ server = {{ autostart = false }} }})",
            file.display()
        ),
        Some(&listen),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let mut claude = claude_for(&home).await;
    wait_until("nvim listens", || listen.exists()).await;
    let real = std::fs::canonicalize(&file).unwrap();

    // The whole file: from the first line to the last (0-based, the last one included).
    send_keys(&listen, ":ClaudeIdeMention<CR>");
    let whole = next_notification(&mut claude, "at_mentioned").await;
    assert_eq!(whole["filePath"], real.to_string_lossy().as_ref());
    assert_eq!(whole["lineStart"], 0);
    assert_eq!(whole["lineEnd"], 3);

    // Some lines: the ones of the range of the command (1-based there, 0-based here).
    send_keys(&listen, ":2,3ClaudeIdeMention<CR>");
    let lines = next_notification(&mut claude, "at_mentioned").await;
    assert_eq!(lines["lineStart"], 1);
    assert_eq!(lines["lineEnd"], 2);
}

#[tokio::test]
async fn claude_code_can_open_a_file_check_whether_it_has_changes_and_save_it() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let project = home.dir.path().join("project");
    std::fs::create_dir_all(project.join("lib")).unwrap();
    std::fs::write(project.join("lib/a.lua"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(project.join("b.txt"), "other\n").unwrap();
    // The workspace of the adapter is the project: it starts there, and a relative path is taken from it.
    let listen = home.dir.path().join("nvim.sock");
    let _nvim = start_nvim_in(
        &home,
        &project,
        "require('claude_ide').setup({ selection_debounce_ms = 20, server = { autostart = false } })",
        Some(&listen),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let mut claude = claude_for(&home).await;
    let real = std::fs::canonicalize(&project).unwrap();
    let a = real.join("lib/a.lua");

    // Not open yet.
    let state = call_with(
        &mut claude,
        "checkDocumentDirty",
        serde_json::json!({ "filePath": "lib/a.lua" }),
    )
    .await;
    assert_eq!(state["success"], false, "{state}");
    assert_eq!(
        state["message"],
        format!("Document not open: {}", a.display()).as_str()
    );

    // Opened by a relative path, with a text to select: the text is found and the file is the frontmost one.
    let opened = call_text(
        &mut claude,
        "openFile",
        serde_json::json!({ "filePath": "lib/a.lua", "startText": "two" }),
    )
    .await;
    assert_eq!(opened, "Opened file and selected text \"two\"");
    // The text is really selected (visual mode is entered as the keys that were typed are processed, a moment later).
    let mut selected = Value::Null;
    for _ in 0..100 {
        selected = call(&mut claude, "getCurrentSelection").await;
        if selected["text"] == "two" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(selected["text"], "two", "{selected}");
    assert_eq!(selected["selection"]["start"]["line"], 1);
    assert_eq!(selected["selection"]["end"]["character"], 3);
    let state = call_with(
        &mut claude,
        "checkDocumentDirty",
        serde_json::json!({ "filePath": a.to_string_lossy() }),
    )
    .await;
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(state["isDirty"], false);
    assert_eq!(state["isUntitled"], false);

    // A text that is not there, and a file that is not.
    let missing = call_text(
        &mut claude,
        "openFile",
        serde_json::json!({ "filePath": "lib/a.lua", "startText": "nowhere" }),
    )
    .await;
    assert_eq!(missing, "Opened file, but text \"nowhere\" not found");
    let gone = claude
        .request(
            "tools/call",
            serde_json::json!({ "name": "openFile", "arguments": { "filePath": "nothing.lua" } }),
        )
        .await;
    assert_eq!(gone["result"]["isError"], true, "{gone}");
    assert!(
        gone["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("File not found"),
        "{gone}"
    );

    // Not made the frontmost one: the details, as JSON.
    let details = call_with(
        &mut claude,
        "openFile",
        serde_json::json!({ "filePath": "b.txt", "makeFrontmost": false }),
    )
    .await;
    assert_eq!(details["success"], true, "{details}");
    assert_eq!(details["languageId"], "plaintext");
    assert_eq!(details["lineCount"], 2);

    // A change that is not written is told, and is written by saveDocument.
    wait_until("nvim listens", || listen.exists()).await;
    send_keys(&listen, "<Esc>ggdd");
    let mut dirty = Value::Null;
    for _ in 0..100 {
        dirty = call_with(
            &mut claude,
            "checkDocumentDirty",
            serde_json::json!({ "filePath": a.to_string_lossy() }),
        )
        .await;
        if dirty["isDirty"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(dirty["isDirty"], true, "{dirty}");
    let saved = call_with(
        &mut claude,
        "saveDocument",
        serde_json::json!({ "filePath": "lib/a.lua" }),
    )
    .await;
    assert_eq!(saved["saved"], true, "{saved}");
    assert_eq!(saved["message"], "Document saved successfully");
    assert_eq!(
        std::fs::read_to_string(project.join("lib/a.lua")).unwrap(),
        "two\nthree\n"
    );
    let again = call_with(
        &mut claude,
        "saveDocument",
        serde_json::json!({ "filePath": "lib/a.lua" }),
    )
    .await;
    assert_eq!(again["saved"], false, "nothing to write the second time");
}

#[tokio::test]
async fn changed_diagnostics_reach_claude_code_once_for_a_burst_of_changes() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let first = home.dir.path().join("a.txt");
    let second = home.dir.path().join("b.lua");
    std::fs::write(&first, "one\n").unwrap();
    std::fs::write(&second, "return 1\n").unwrap();
    let listen = home.dir.path().join("nvim.sock");
    let _nvim = start_nvim_listening(
        &home,
        &format!(
            "vim.cmd('edit {a}'); vim.fn.bufload(vim.fn.bufadd('{b}')); \
             require('claude_ide').setup({{ diagnostics_debounce_ms = 100, server = {{ autostart = false }} }})",
            a = first.display(),
            b = second.display()
        ),
        Some(&listen),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let mut claude = claude_for(&home).await;
    wait_until("nvim listens", || listen.exists()).await;
    let uri = |path: &Path| format!("file://{}", std::fs::canonicalize(path).unwrap().display());

    // The diagnostics of two files change, and the first one again, within the wait: one notification names both files,
    // each once, in the order in which they changed.
    send_keys(
        &listen,
        &format!(
            ":lua local ns = vim.api.nvim_create_namespace('t'); \
             local a, b = vim.fn.bufnr('{a}'), vim.fn.bufnr('{b}'); \
             vim.diagnostic.set(ns, a, {{ {{ lnum = 0, col = 0, message = 'x' }} }}); \
             vim.diagnostic.set(ns, b, {{ {{ lnum = 0, col = 0, message = 'y' }} }}); \
             vim.diagnostic.set(ns, a, {{ {{ lnum = 0, col = 1, message = 'z' }} }})<CR>",
            a = first.display(),
            b = second.display()
        ),
    );
    let changed = next_notification(&mut claude, "diagnostics_changed").await;
    assert_eq!(
        changed["uris"],
        serde_json::json!([uri(&first), uri(&second)])
    );

    // A later change is another notification, with only the file that changed.
    send_keys(
        &listen,
        &format!(
            ":lua vim.diagnostic.reset(vim.api.nvim_create_namespace('t'), vim.fn.bufnr('{b}'))<CR>",
            b = second.display()
        ),
    );
    let changed = next_notification(&mut claude, "diagnostics_changed").await;
    assert_eq!(changed["uris"], serde_json::json!([uri(&second)]));
}

/// The number of tab pages that a Neovim has.
fn tab_pages(listen: &Path) -> u32 {
    let output = Command::new("nvim")
        .arg("--server")
        .arg(listen)
        .args(["--remote-expr", "tabpagenr('$')"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)))
}

async fn wait_for_tab_pages(listen: &Path, expected: u32) {
    let mut seen = 0;
    for _ in 0..150 {
        seen = tab_pages(listen);
        if seen == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{expected} tab pages expected, {seen} seen");
}

/// `tools/call` that does not end until the user has decided: in a task of its own, which gives the Claude Code back.
fn call_in_task(
    mut claude: MockClaude,
    name: &str,
    arguments: Value,
) -> tokio::task::JoinHandle<(MockClaude, Value)> {
    let name = name.to_owned();
    tokio::spawn(async move {
        let answer = claude
            .request(
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            )
            .await;
        (claude, answer)
    })
}

fn texts(answer: &Value) -> Vec<String> {
    answer["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("no content in {answer}"))
        .iter()
        .map(|item| item["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn claude_code_can_propose_a_change_that_the_user_accepts_edits_or_rejects() {
    if !nvim_available() {
        return;
    }
    let home = Home::new();
    let _server = start_server(&home);
    let _ = connect_when_ready(&home.default_socket()).await;

    let file = home.dir.path().join("a.lua");
    std::fs::write(&file, "return 1\n").unwrap();
    let listen = home.dir.path().join("nvim.sock");
    let _nvim = start_nvim_listening(
        &home,
        &format!(
            "vim.cmd('edit {}'); require('claude_ide').setup({{ server = {{ autostart = false }} }})",
            file.display()
        ),
        Some(&listen),
    );
    wait_until("the adapter registers", || !home.lock_files().is_empty()).await;
    let claude = claude_for(&home).await;
    wait_until("nvim listens", || listen.exists()).await;
    let arguments = |tab_name: &str| {
        serde_json::json!({
            "old_file_path": file.to_string_lossy(),
            "new_file_path": file.to_string_lossy(),
            "new_file_contents": "return 2\n",
            "tab_name": tab_name,
        })
    };
    assert_eq!(tab_pages(&listen), 1);

    // The diff is shown in a tab page of its own and waits; the user edits the proposal and writes it: accepted, with
    // the contents as they are then. (Nothing is written to the file: Claude Code does that.)
    let call = call_in_task(claude, "openDiff", arguments("edit-1"));
    wait_for_tab_pages(&listen, 2).await;
    // The user edits the proposal: the whole first line is replaced.
    send_keys(&listen, "<Esc>ggcc-- edited<Esc>:write<CR>");
    let (claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert_eq!(texts(&result), ["FILE_SAVED", "-- edited\n"]);
    wait_for_tab_pages(&listen, 1).await;
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "return 1\n",
        "the file is Claude Code's to write"
    );

    // Rejected by the command.
    let call = call_in_task(claude, "openDiff", arguments("edit-2"));
    wait_for_tab_pages(&listen, 2).await;
    send_keys(&listen, "<Esc>:ClaudeIdeDiffReject<CR>");
    let (claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(texts(&result), ["DIFF_REJECTED", "edit-2"]);
    wait_for_tab_pages(&listen, 1).await;

    // Rejected by closing the tab page.
    let call = call_in_task(claude, "openDiff", arguments("edit-3"));
    wait_for_tab_pages(&listen, 2).await;
    send_keys(&listen, "<Esc>:tabclose<CR>");
    let (mut claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(texts(&result), ["DIFF_REJECTED", "edit-3"]);
    assert_eq!(tab_pages(&listen), 1);

    // Closed from Claude Code: the call that waits ends rejected, and `closeAllDiffTabs` counts what it closed.
    let other = another_claude_for(&home).await;
    let call = call_in_task(other, "openDiff", arguments("edit-4"));
    wait_for_tab_pages(&listen, 2).await;
    let closed = call_text(
        &mut claude,
        "close_tab",
        serde_json::json!({ "tab_name": "edit-4" }),
    )
    .await;
    assert_eq!(closed, "TAB_CLOSED");
    let (other, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(texts(&result), ["DIFF_REJECTED", "edit-4"]);
    wait_for_tab_pages(&listen, 1).await;

    let call_a = call_in_task(other, "openDiff", arguments("a"));
    wait_for_tab_pages(&listen, 2).await;
    let all = call_text(&mut claude, "closeAllDiffTabs", serde_json::json!({})).await;
    assert_eq!(all, "CLOSED_1_DIFF_TABS");
    let (_other, result) = timeout(WAIT, call_a).await.unwrap().unwrap();
    assert_eq!(texts(&result), ["DIFF_REJECTED", "a"]);
    wait_for_tab_pages(&listen, 1).await;
}

/// Another Claude Code, connected to the workspace that there is.
async fn another_claude_for(home: &Home) -> MockClaude {
    claude_for(home).await
}
