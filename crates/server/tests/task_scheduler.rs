//! The server under Task Scheduler, with the real one: the task that `contrib/windows` ships is registered, started, and stopped, with PowerShell, as the file says.
//!
//! A task cannot be given an environment, so the server runs with the paths of the user (the default pipe, and
//! `~/.claude/ide` for the lock files). That is no place for a test to run on a machine where the user has a server of
//! their own, so it does nothing unless `CLAUDE_IDE_TRY_TASK_SCHEDULER=1` is set, which is what CI does.

#![cfg(windows)]

use claude_ide_test_support as common;
mod support;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use claude_ide_core::lock::{EnvVars, LockContents, LockDirs};
use claude_ide_core::workspace::socket_path;
use common::claude::MockClaude;
use common::{Client, connect_to_adapters, registration};
use serde_json::json;
use support::WAIT;
use tokio::time::timeout;

const SHIPPED: &str = include_str!("../../../contrib/windows/claude-ide-server.xml");

/// A command of PowerShell, which is how the file that is shipped says that it is installed: whether it succeeded, and
/// what it said.
fn powershell(command: &str) -> (bool, String) {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .output()
        .unwrap();
    let text = format!(
        "powershell {command}: {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

/// What Task Scheduler says about the task: its state, and how it ended the last time.
fn state(name: &str) -> String {
    powershell(&format!(
        "$task = Get-ScheduledTask -TaskName '{name}'; $info = Get-ScheduledTaskInfo -TaskName '{name}'; \
         \"state: $($task.State); last result: $($info.LastTaskResult); last run: $($info.LastRunTime)\""
    ))
    .1
}

/// A task that is stopped and taken out when this is dropped.
struct Task(String);

impl Drop for Task {
    fn drop(&mut self) {
        let _ = powershell(&format!("Stop-ScheduledTask -TaskName '{}'", self.0));
        let _ = powershell(&format!(
            "Unregister-ScheduledTask -TaskName '{}' -Confirm:$false",
            self.0
        ));
    }
}

/// The task that is shipped, with the path of the executable that was built and no idle exit filled in.
fn definition() -> String {
    let from = r"C:\Program Files\claude-ide-server\claude-ide-server.exe";
    assert_eq!(SHIPPED.matches(from).count(), 1, "the path in the task");
    SHIPPED.replacen(from, env!("CARGO_BIN_EXE_claude-ide-server"), 1)
}

fn lock_files(dirs: &LockDirs) -> Vec<(PathBuf, LockContents)> {
    let Ok(entries) = std::fs::read_dir(&dirs.lock_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let contents = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            Some((path, contents))
        })
        .collect()
}

#[tokio::test]
async fn stopping_the_task_ends_the_server_and_leaves_no_lock_file() {
    if std::env::var_os("CLAUDE_IDE_TRY_TASK_SCHEDULER").is_none() {
        eprintln!("skipped: CLAUDE_IDE_TRY_TASK_SCHEDULER is not set");
        return;
    }
    let env = EnvVars::from_process();
    let pipe = socket_path(&env);
    let dirs = LockDirs::from_env(&env).unwrap();
    assert!(
        connect_to_adapters(&pipe).await.is_err(),
        "a server is listening on {} already",
        pipe.display()
    );

    let name = format!("claude-ide-server-test-{}", std::process::id());
    let xml = std::env::temp_dir().join(format!("{name}.xml"));
    std::fs::write(&xml, definition()).unwrap();
    let (created, said) = powershell(&format!(
        "Register-ScheduledTask -TaskName '{name}' -Xml (Get-Content -Raw '{}') | Out-Null",
        xml.display()
    ));
    let _ = std::fs::remove_file(&xml);
    assert!(created, "{said}");
    let task = Task(name.clone());

    let (started, said) = powershell(&format!("Start-ScheduledTask -TaskName '{name}'"));
    assert!(started, "{said}");

    // The server is there when the pipe can be opened: an adapter registers, and Claude Code finds the lock file.
    let mut adapter = Client::new(
        timeout(WAIT, async {
            loop {
                if let Ok(stream) = connect_to_adapters(&pipe).await {
                    return stream;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the task did not start a server:\n{}", state(&name))),
    );
    let port = adapter.register(registration()).await.unwrap();
    let ours = lock_files(&dirs);
    let (_, lock) = ours
        .iter()
        .find(|(path, _)| {
            path.file_stem()
                .is_some_and(|stem| stem.to_string_lossy() == port.to_string())
        })
        .expect("the lock file of the port");
    let mut claude = MockClaude::connect(port, &lock.auth_token).await;
    claude.handshake(4242).await;
    let answer = claude
        .request(
            "tools/call",
            json!({"name": "getWorkspaceFolders", "arguments": {}}),
        )
        .await;
    assert!(
        answer.to_string().contains("/home/user/project"),
        "{answer}"
    );

    // Stopping the task: Task Scheduler kills the process, which gets no chance to clean up.
    let (stopped, said) = powershell(&format!("Stop-ScheduledTask -TaskName '{name}'"));
    assert!(stopped, "{said}");
    let gone = timeout(WAIT, async {
        while connect_to_adapters(&pipe).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
    assert!(gone, "the pipe is still there:\n{}", state(&name));
    // The lock file was held by a handle that deletes it when it is closed, which the system did when the process ended.
    let left = lock_files(&dirs)
        .iter()
        .filter(|(_, contents)| contents.auth_token == lock.auth_token)
        .count();
    assert_eq!(
        left,
        0,
        "stopping the task left the lock file behind:\n{}",
        state(&name)
    );
    drop(task);
}
