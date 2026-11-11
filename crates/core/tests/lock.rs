//! Tests of the lock files, in temporary directories. Nothing here touches the real `~/.claude`.

use std::fs;
use std::path::PathBuf;

use claude_ide_core::lock::{
    EnvVars, LockContents, LockDirs, LockFile, TRANSPORT_WS, cleanup_stale, generate_token,
    process_is_alive,
};
use tempfile::TempDir;

mod common;
use common::shape::assert_shape;

const SERVER_PID: u32 = 4242;

fn dirs(root: &TempDir) -> LockDirs {
    LockDirs {
        lock_dir: root.path().join("claude").join("ide"),
        record_dir: root.path().join("run").join("claude-ide-server"),
    }
}

fn contents(token: &str) -> LockContents {
    LockContents {
        pid: 1234,
        workspace_folders: vec!["/home/user/project".to_owned()],
        ide_name: "Neovim".to_owned(),
        transport: TRANSPORT_WS.to_owned(),
        running_in_windows: false,
        auth_token: token.to_owned(),
    }
}

/// The permission bits of a file or directory (Unix; on Windows what a file gets is what its directory gives it).
#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_file_has_the_name_the_contents_and_the_modes_that_the_cli_expects() {
    let root = TempDir::new().unwrap();
    let dirs = dirs(&root);
    let token = generate_token().unwrap();
    let lock = LockFile::create(&dirs, 41234, contents(&token), SERVER_PID).unwrap();

    assert_eq!(lock.path(), dirs.lock_dir.join("41234.lock"));
    assert_eq!(names(&dirs.lock_dir), ["41234.lock"]);
    #[cfg(unix)]
    {
        assert_eq!(mode(lock.path()), 0o600);
        assert_eq!(mode(&dirs.lock_dir), 0o700);
        assert_eq!(mode(&dirs.record_dir), 0o700);
    }

    // The keys and their types, as the CLI reads them (and nothing besides them).
    let json: serde_json::Value = serde_json::from_slice(&fs::read(lock.path()).unwrap()).unwrap();
    assert_shape(&json, "lock_file.shape.json");
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "authToken",
            "ideName",
            "pid",
            "runningInWindows",
            "transport",
            "workspaceFolders"
        ]
    );
    assert_eq!(json["pid"], 1234);
    assert_eq!(json["transport"], "ws");
    assert_eq!(json["ideName"], "Neovim");
    assert_eq!(json["runningInWindows"], false);
    assert_eq!(json["workspaceFolders"][0], "/home/user/project");
    assert_eq!(json["authToken"], token);
}

#[test]
fn a_token_is_32_lowercase_hex_characters_and_not_repeated() {
    let first = generate_token().unwrap();
    let second = generate_token().unwrap();
    for token in [&first, &second] {
        assert_eq!(token.len(), 32);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }
    assert_ne!(first, second);
}

#[test]
fn rewriting_changes_the_folders_and_leaves_no_temporary_file() {
    let root = TempDir::new().unwrap();
    let dirs = dirs(&root);
    let mut lock = LockFile::create(&dirs, 41235, contents("t"), SERVER_PID).unwrap();

    lock.set_workspace_folders(vec!["/a".to_owned(), "/b".to_owned()])
        .unwrap();

    let on_disk: LockContents = serde_json::from_slice(&fs::read(lock.path()).unwrap()).unwrap();
    assert_eq!(on_disk.workspace_folders, ["/a", "/b"]);
    assert_eq!(on_disk, *lock.contents());
    assert_eq!(names(&dirs.lock_dir), ["41235.lock"]);
    #[cfg(unix)]
    assert_eq!(mode(lock.path()), 0o600);
}

#[test]
fn closing_and_dropping_remove_the_file_and_its_record() {
    let root = TempDir::new().unwrap();
    let dirs = dirs(&root);

    let mut lock = LockFile::create(&dirs, 41236, contents("t"), SERVER_PID).unwrap();
    assert_eq!(names(&dirs.record_dir), ["41236.json"]);
    lock.close().unwrap();
    lock.close().unwrap(); // twice is fine
    assert!(names(&dirs.lock_dir).is_empty());
    assert!(names(&dirs.record_dir).is_empty());

    {
        let _lock = LockFile::create(&dirs, 41237, contents("t"), SERVER_PID).unwrap();
        assert_eq!(names(&dirs.lock_dir), ["41237.lock"]);
    }
    assert!(names(&dirs.lock_dir).is_empty());
    assert!(names(&dirs.record_dir).is_empty());
}

#[test]
fn after_a_crash_the_next_start_removes_the_leftover_file() {
    let root = TempDir::new().unwrap();
    let dirs = dirs(&root);
    let lock = LockFile::create(&dirs, 41238, contents("secret"), SERVER_PID).unwrap();
    std::mem::forget(lock); // the server died: nothing ran Drop

    // The recorded server is still alive: nothing is touched.
    assert!(cleanup_stale(&dirs, |_| true).unwrap().is_empty());
    assert_eq!(names(&dirs.lock_dir), ["41238.lock"]);

    // The recorded server is gone: the file and the record are removed.
    let removed = cleanup_stale(&dirs, |pid| pid != SERVER_PID).unwrap();
    assert_eq!(removed, [dirs.lock_dir.join("41238.lock")]);
    assert!(names(&dirs.lock_dir).is_empty());
    assert!(names(&dirs.record_dir).is_empty());
}

#[test]
fn a_file_that_something_else_wrote_is_not_removed() {
    let root = TempDir::new().unwrap();
    let dirs = dirs(&root);
    let lock = LockFile::create(&dirs, 41239, contents("ours"), SERVER_PID).unwrap();
    let path = lock.path().to_owned();
    std::mem::forget(lock);
    // Another program has taken the name since: same port, another token.
    fs::write(
        &path,
        serde_json::to_vec(&contents("someone else's")).unwrap(),
    )
    .unwrap();

    assert!(cleanup_stale(&dirs, |_| false).unwrap().is_empty());
    assert_eq!(names(&dirs.lock_dir), ["41239.lock"]);
    // The record is gone all the same: it has nothing left to protect.
    assert!(names(&dirs.record_dir).is_empty());
}

#[test]
fn cleanup_copes_with_a_missing_directory_a_missing_lock_and_a_broken_record() {
    let root = TempDir::new().unwrap();
    let dirs = dirs(&root);
    // No record directory at all.
    assert!(cleanup_stale(&dirs, |_| false).unwrap().is_empty());

    // A record whose lock file is already gone, and a record that is not JSON.
    let lock = LockFile::create(&dirs, 41240, contents("t"), SERVER_PID).unwrap();
    fs::remove_file(lock.path()).unwrap();
    std::mem::forget(lock);
    fs::write(dirs.record_dir.join("41241.json"), b"not json").unwrap();
    fs::write(dirs.record_dir.join("notes.txt"), b"left alone").unwrap();

    assert!(cleanup_stale(&dirs, |_| false).unwrap().is_empty());
    assert_eq!(names(&dirs.record_dir), ["notes.txt"]);
}

#[test]
fn directories_follow_the_environment() {
    let env = |config: Option<&str>, home: Option<&str>, xdg: Option<&str>| EnvVars {
        claude_config_dir: config.map(PathBuf::from),
        home: home.map(PathBuf::from),
        xdg_runtime_dir: xdg.map(PathBuf::from),
        user: "1000".into(),
    };

    let from_config =
        LockDirs::from_env(&env(Some("/cfg"), Some("/home/u"), Some("/run/u"))).unwrap();
    // The expected paths are built the way a path is on the system that runs the test (`/` or `\\` between the parts).
    assert_eq!(from_config.lock_dir, PathBuf::from("/cfg").join("ide"));
    assert_eq!(
        from_config.record_dir,
        PathBuf::from("/run/u").join("claude-ide-server")
    );

    let from_home = LockDirs::from_env(&env(None, Some("/home/u"), None)).unwrap();
    assert_eq!(
        from_home.lock_dir,
        PathBuf::from("/home/u").join(".claude").join("ide")
    );
    assert_eq!(
        from_home.record_dir,
        std::env::temp_dir().join("claude-ide-server-1000")
    );

    assert!(LockDirs::from_env(&env(None, None, None)).is_err());
}

#[test]
fn liveness_tells_this_process_from_a_gone_one() {
    assert!(process_is_alive(std::process::id()));
    // A child that has exited and been reaped: its id no longer names a process.
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "exit", "0"]);
        command
    } else {
        std::process::Command::new("true")
    };
    let mut child = command.spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(!process_is_alive(pid));
    assert!(!process_is_alive(0));
}
