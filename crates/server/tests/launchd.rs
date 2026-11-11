//! The server under launchd, with a real launchd: the socket is launchd's, nothing runs until an adapter connects, and
//! when the server has been idle and ends, the socket stays and the next connection starts it again.
//!
//! This needs a domain of launchd that a user agent can be loaded into (`gui/<uid>`, or else `user/<uid>`). A machine that
//! has none (a session without a user logged in, a container) cannot run it: the test says so and ends, unless
//! `CLAUDE_IDE_REQUIRE_LAUNCHD=1` is set, which is what CI does, so that a runner that cannot do it is noticed.

#![cfg(target_os = "macos")]

// The adapter end is that of the core's own tests.
#[path = "../../core/tests/common/mod.rs"]
mod common;
mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Client, registration};
use support::{Home, WAIT, connect_when_ready, wait_for_log};
use tokio::net::UnixStream;
use tokio::time::timeout;

fn uid() -> String {
    let output = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A job that is loaded into a domain of launchd, and is taken out again (and its property list removed) when this is dropped.
struct Agent {
    target: String,
    plist: PathBuf,
}

impl Agent {
    /// The command of `launchctl`, with what it said, for a message.
    fn launchctl(arguments: &[&str]) -> (bool, String) {
        let output = Command::new("launchctl").args(arguments).output().unwrap();
        let text = format!(
            "launchctl {}: {}\n{}{}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }

    /// Loads the property list into the first domain that takes it, or says what each one answered.
    fn load(plist: &Path, label: &str) -> Result<Self, String> {
        let mut said = String::new();
        for domain in [format!("gui/{}", uid()), format!("user/{}", uid())] {
            let (ok, text) = Self::launchctl(&["bootstrap", &domain, &plist.to_string_lossy()]);
            if ok {
                return Ok(Self {
                    target: format!("{domain}/{label}"),
                    plist: plist.to_owned(),
                });
            }
            said.push_str(&text);
        }
        Err(said)
    }

    /// What launchd says about the job (its state, and what it has run).
    fn print(&self) -> String {
        Self::launchctl(&["print", &self.target]).1
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = Self::launchctl(&["bootout", &self.target]);
        let _ = std::fs::remove_file(&self.plist);
    }
}

/// The property list that `contrib/launchd` ships, made into a job of this test: another label, the executable that
/// was built, a second of idle time, and the environment (all of it under `home`) and the log of the test. Each of the
/// places that are changed is found exactly once, so that a change of the file is noticed here.
fn property_list(home: &Home, label: &str) -> String {
    const SHIPPED: &str =
        include_str!("../../../contrib/launchd/dev.small-gear-lab.claude-ide-server.plist");
    fn replaced(text: &str, from: &str, to: &str) -> String {
        assert_eq!(
            text.matches(from).count(),
            1,
            "{from:?} in the property list"
        );
        text.replacen(from, to, 1)
    }
    let dir = home.dir.path();
    let text = replaced(
        SHIPPED,
        "<string>dev.small-gear-lab.claude-ide-server</string>",
        &format!("<string>{label}</string>"),
    );
    let text = replaced(
        &text,
        "<string>/usr/local/bin/claude-ide-server</string>",
        &format!(
            "<string>{}</string>",
            env!("CARGO_BIN_EXE_claude-ide-server")
        ),
    );
    let text = replaced(&text, "<string>300</string>", "<string>1</string>");
    // The socket is filled in as the instructions in the file say; it is in the comment too, which does no harm.
    let text = text.replace("@SOCKET@", &home.socket().display().to_string());
    replaced(
        &text,
        "</dict>\n</plist>",
        &format!(
            r#"  <key>EnvironmentVariables</key>
  <dict>
    <key>CLAUDE_CONFIG_DIR</key><string>{claude}</string>
    <key>XDG_RUNTIME_DIR</key><string>{xdg}</string>
    <key>HOME</key><string>{home}</string>
  </dict>
  <key>ThrottleInterval</key><integer>1</integer>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>"#,
            claude = dir.join("claude").display(),
            xdg = dir.join("xdg").display(),
            home = dir.display(),
            log = home.log_path().display(),
        ),
    )
}

/// What an adapter does: connects (which is what makes launchd start the server), registers, and gets the port.
async fn register(socket: &Path) -> Client<UnixStream> {
    let mut adapter = Client::new(connect_when_ready(socket).await);
    adapter.register(registration()).await.unwrap();
    adapter
}

#[tokio::test]
async fn nothing_runs_until_an_adapter_connects_and_the_socket_stays_when_the_server_ends_idle() {
    let home = Home::new();
    std::fs::create_dir_all(home.socket().parent().unwrap()).unwrap();
    let label = format!(
        "dev.small-gear-lab.claude-ide-server.test{}",
        std::process::id()
    );
    let plist = home.dir.path().join(format!("{label}.plist"));
    std::fs::write(&plist, property_list(&home, &label)).unwrap();
    let lint = Command::new("plutil")
        .arg("-lint")
        .arg(&plist)
        .output()
        .unwrap();
    assert!(
        lint.status.success(),
        "plutil: {}{}",
        String::from_utf8_lossy(&lint.stdout),
        String::from_utf8_lossy(&lint.stderr)
    );

    let agent = match Agent::load(&plist, &label) {
        Ok(agent) => agent,
        Err(said) => {
            assert!(
                std::env::var_os("CLAUDE_IDE_REQUIRE_LAUNCHD").is_none(),
                "no domain of launchd takes the job:\n{said}"
            );
            eprintln!("skipped: no domain of launchd takes the job:\n{said}");
            return;
        }
    };

    // Loaded, and nothing has run: the log of the server is not there yet, and launchd does not say that it runs.
    assert!(home.log().is_empty(), "the server ran:\n{}", home.log());
    assert!(
        !agent.print().contains("state = running"),
        "the server runs:\n{}",
        agent.print()
    );
    assert!(
        home.socket().exists(),
        "launchd made the socket:\n{}",
        agent.print()
    );

    // The first connection starts it, and the socket that it serves is the one that launchd handed over.
    let adapter = register(&home.socket()).await;
    assert!(
        home.log().contains("handed over by launchd"),
        "{}\n{}",
        home.log(),
        agent.print()
    );

    // With no adapter for a second, it ends by itself; the socket is launchd's, and stays.
    drop(adapter);
    wait_for_log(&home, "no adapter for 1 seconds").await;
    wait_for_log(&home, "stopped").await;
    assert!(home.socket().exists());

    // And the next connection starts it again.
    let again = timeout(WAIT + Duration::from_secs(5), register(&home.socket()))
        .await
        .unwrap_or_else(|_| panic!("a second start:\n{}\n{}", home.log(), agent.print()));
    drop(again);
}
