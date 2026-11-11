//! A probe for the Claude Code side: it listens like an IDE, writes a lock file, and logs every message that a
//! connected CLI sends, answering `initialize` and `tools/list` minimally.
//!
//! It is for looking at what a real CLI says and expects (the prototype of issue #5), not part of the server.
//!
//!     cargo run --example mcp_probe -- <workspace-folder> [log-file]
//!
//! Then start the CLI inside that folder with `CLAUDE_CODE_SSE_PORT=<the port that is printed>`.

use std::io::Write;
use std::path::PathBuf;

use claude_ide_core::lock::{self, EnvVars, LockContents, LockDirs, LockFile};
use claude_ide_core::ws::{self, WsServer};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let folder = args
        .next()
        .expect("usage: mcp_probe <workspace-folder> [log-file]");
    let log_path = args.next();

    let dirs = LockDirs::from_env(&EnvVars::from_process())?;
    let token = lock::generate_token()?;
    let mut server = WsServer::start(token.clone(), ws::Config::default()).await?;
    let port = server.port();
    let _lock = LockFile::create(
        &dirs,
        port,
        LockContents {
            pid: std::process::id(),
            workspace_folders: vec![folder.clone()],
            ide_name: "claude-ide-server probe".to_owned(),
            transport: lock::TRANSPORT_WS.to_owned(),
            running_in_windows: false,
            auth_token: token,
        },
        std::process::id(),
    )?;
    let mut log =
        log_path.map(|path| std::fs::File::create(PathBuf::from(path)).expect("the log file"));
    let mut say = move |line: String| {
        eprintln!("{line}");
        if let Some(file) = log.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    };
    say(format!(
        "listening port={port} lock={}/{port}.lock",
        dirs.lock_dir.display()
    ));

    while let Some(mut connection) = server.accept().await {
        say("--- a client connected".to_owned());
        while let Some(received) = connection.recv().await {
            let text = match received {
                Ok(text) => text,
                Err(error) => {
                    say(format!("<- error: {error}"));
                    break;
                }
            };
            say(format!("<- {text}"));
            let Ok(message) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let (Some(method), Some(id)) = (message["method"].as_str(), message.get("id")) else {
                continue;
            };
            let result = match method {
                "initialize" => Ok(json!({
                    "protocolVersion": message["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "claude-ide-server probe", "version": "0.0.0" },
                })),
                "tools/list" => Ok(json!({ "tools": [] })),
                "ping" => Ok(json!({})),
                other => Err(json!({ "code": -32601, "message": format!("no method {other}") })),
            };
            let reply = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
            };
            say(format!("-> {reply}"));
            if connection.send(reply.to_string()).await.is_err() {
                break;
            }
        }
        say("--- the client went away".to_owned());
    }
    Ok(())
}
