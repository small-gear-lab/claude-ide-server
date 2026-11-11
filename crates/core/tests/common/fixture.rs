//! A workspace server over in-memory streams, with the lock files in a temporary directory, and what the tests of the
//! workspaces and of the MCP layer do with it.

use std::fs;

use claude_ide_core::adapter;
use claude_ide_core::adapter::memory::{MemoryConnector, listener};
use claude_ide_core::lock::{LockContents, LockDirs};
use claude_ide_core::workspace::{Config, Workspace, WorkspaceServer};
use claude_ide_core::ws::AUTH_HEADER;
use tempfile::TempDir;
use tokio::io::DuplexStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::{Client, WAIT, registration};

pub struct Fixture {
    // Held so that the temporary directory lives as long as the fixture.
    _root: TempDir,
    pub dirs: LockDirs,
    pub connector: MemoryConnector,
    pub server: WorkspaceServer,
}

impl Fixture {
    pub fn new() -> Self {
        let root = TempDir::new().unwrap();
        let dirs = LockDirs {
            lock_dir: root.path().join("ide"),
            record_dir: root.path().join("run"),
        };
        Self::in_dirs(root, dirs)
    }

    pub fn in_dirs(root: TempDir, dirs: LockDirs) -> Self {
        Self::with_adapter_config(root, dirs, adapter::Config::default())
    }

    /// Like [`new`](Self::new), with another time that a request to the adapter may take.
    pub fn with_request_timeout(request_timeout: std::time::Duration) -> Self {
        let root = TempDir::new().unwrap();
        let dirs = LockDirs {
            lock_dir: root.path().join("ide"),
            record_dir: root.path().join("run"),
        };
        let config = adapter::Config {
            request_timeout,
            ..adapter::Config::default()
        };
        Self::with_adapter_config(root, dirs, config)
    }

    /// Like [`new`](Self::new), with another time that Claude Code may take to complete the MCP handshake.
    pub fn with_claude_handshake_timeout(timeout: std::time::Duration) -> Self {
        let root = TempDir::new().unwrap();
        let dirs = LockDirs {
            lock_dir: root.path().join("ide"),
            record_dir: root.path().join("run"),
        };
        let mut config = Config::new(dirs.clone(), std::process::id());
        config.claude_handshake_timeout = timeout;
        Self::started(root, dirs, adapter::Config::default(), config)
    }

    fn with_adapter_config(root: TempDir, dirs: LockDirs, adapter_config: adapter::Config) -> Self {
        let config = Config::new(dirs.clone(), std::process::id());
        Self::started(root, dirs, adapter_config, config)
    }

    fn started(
        root: TempDir,
        dirs: LockDirs,
        adapter_config: adapter::Config,
        config: Config,
    ) -> Self {
        let (listener, connector) = listener();
        let server = WorkspaceServer::start(listener, adapter_config, config);
        Self {
            _root: root,
            dirs,
            connector,
            server,
        }
    }

    pub async fn client(&self) -> Client<DuplexStream> {
        Client::new(self.connector.connect().await.unwrap())
    }

    pub fn lock_files(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dirs.lock_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    pub fn read_lock(&self, port: u16) -> LockContents {
        serde_json::from_slice(&fs::read(self.dirs.lock_dir.join(format!("{port}.lock"))).unwrap())
            .unwrap()
    }
}

pub async fn registered(fixture: &mut Fixture) -> (Client<DuplexStream>, Workspace, u16) {
    let mut client = fixture.client().await;
    let port = client.register(registration()).await.unwrap();
    let workspace = timeout(WAIT, fixture.server.accept())
        .await
        .unwrap()
        .expect("a workspace");
    (client, workspace, port)
}

pub async fn claude_connect(port: u16, token: &str) -> Result<(), WsError> {
    let mut request = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTH_HEADER, token.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.map(|_| ())
}
