//! Paths and the `file://` URIs of them, in the way of the system that runs the server (and, on the same machine, the
//! editor): `/` paths on Unix, and on Windows paths with a drive (`C:\Users\u\a.lua`) or a share (`\\server\share\a.lua`).
//!
//! These are functions of strings, which do not look at the file system, so that the Windows way is tested on every
//! system, not only on Windows. [`Flavor::NATIVE`] is the way of the system that this is built for.
//!
//! What is an absolute path is what Claude Code, which is Node, says: a path that starts with a separator, or with a drive
//! and a separator (`path.win32.isAbsolute`). The server does not change the paths that an editor reports (Claude Code
//! compares them as text, after making the drive letter of its own working directory upper case), except for the one thing
//! it must: a relative `filePath` of a tool call is joined with the first folder of the workspace.

/// How the paths of a system are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    /// `/home/u/a.lua`.
    Posix,
    /// `C:\Users\u\a.lua`, `C:/Users/u/a.lua`, `\\server\share\a.lua` (and `/` rooted paths, as Node takes them).
    Windows,
}

impl Flavor {
    /// The way of the system that this is built for.
    pub(crate) const NATIVE: Flavor = if cfg!(windows) {
        Flavor::Windows
    } else {
        Flavor::Posix
    };
}

fn is_separator(character: char, flavor: Flavor) -> bool {
    match flavor {
        Flavor::Posix => character == '/',
        Flavor::Windows => character == '/' || character == '\\',
    }
}

/// The letter of a drive at the start of a path (`C` of `C:\a`), if it has one.
fn drive_letter(path: &str) -> Option<char> {
    let mut characters = path.chars();
    let letter = characters.next().filter(char::is_ascii_alphabetic)?;
    (characters.next() == Some(':')).then_some(letter)
}

/// Whether the path starts at a root: what Claude Code works out what to show from, and what a relative path would not
/// be (it would be taken from the directory that Claude Code runs in, which is not that of the editor).
pub(crate) fn is_absolute(path: &str, flavor: Flavor) -> bool {
    let mut characters = path.chars();
    match characters.next() {
        Some(first) if is_separator(first, flavor) => true,
        Some(_) if flavor == Flavor::Windows && drive_letter(path).is_some() => path[2..]
            .chars()
            .next()
            .is_some_and(|c| is_separator(c, flavor)),
        _ => false,
    }
}

/// The root of a path and what follows it: `C:` and `\a\b` for `C:\a\b`, `\\server\share` and `\a` for `\\server\share\a`,
/// and nothing and the whole path for a path that is rooted at the separator, or is not rooted.
fn split_root(path: &str) -> (&str, &str) {
    if drive_letter(path).is_some() {
        return path.split_at(2);
    }
    let after_separators = path.trim_start_matches(['/', '\\']);
    if path.len() - after_separators.len() == 2 {
        // `\\server\share`: the root is the two names.
        let mut end = 0;
        let mut names = 0;
        for (index, character) in after_separators.char_indices() {
            if character == '/' || character == '\\' {
                names += 1;
                if names == 2 {
                    break;
                }
            }
            end = index + character.len_utf8();
        }
        let root_end = 2 + end;
        return path.split_at(root_end.min(path.len()));
    }
    ("", path)
}

/// `base` and `relative` joined, with `.` and `..` resolved (without looking at the file system), and no `..` going above
/// the root. The separator is the one that `base` uses (`\` if it has one, on Windows, and `/` otherwise).
pub(crate) fn join_normalized(base: &str, relative: &str, flavor: Flavor) -> String {
    match flavor {
        Flavor::Posix => {
            let mut parts: Vec<&str> = Vec::new();
            resolve(&mut parts, base.split('/').chain(relative.split('/')));
            format!("/{}", parts.join("/"))
        }
        Flavor::Windows => {
            let separator = if base.contains('\\') { '\\' } else { '/' };
            let (root, rest) = split_root(base);
            let mut parts: Vec<&str> = Vec::new();
            resolve(
                &mut parts,
                rest.split(['/', '\\']).chain(relative.split(['/', '\\'])),
            );
            let root = root.replace(['/', '\\'], &separator.to_string());
            if parts.is_empty() {
                // The root alone (`C:\`), or the one separator.
                return format!("{root}{separator}");
            }
            let mut joined = root;
            for part in parts {
                joined.push(separator);
                joined.push_str(part);
            }
            joined
        }
    }
}

fn resolve<'a>(parts: &mut Vec<&'a str>, names: impl Iterator<Item = &'a str>) {
    for name in names {
        match name {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
}

/// `file://` and the path, with every byte that is not safe in a URI written as `%XX` (the `/` stays).
///
/// On Windows the backslashes become `/`, a path with a drive gets the third slash (`C:\a b` is `file:///C:/a%20b`, the `:`
/// of the drive stays) and a path of a share has its server as the host (`\\server\share\a` is `file://server/share/a`).
pub(crate) fn file_uri(path: &str, flavor: Flavor) -> String {
    let mut uri = String::from("file://");
    match flavor {
        Flavor::Posix => encode(&mut uri, path),
        Flavor::Windows => {
            let slashed = path.replace('\\', "/");
            // `\\?\C:\a` is how a path that skips the checks of Windows is written: the same path.
            let slashed = slashed.strip_prefix("//?/").unwrap_or(&slashed);
            if let Some(letter) = drive_letter(slashed) {
                uri.push('/');
                uri.push(letter);
                uri.push(':');
                encode(&mut uri, &slashed[2..]);
            } else if let Some(share) = slashed.strip_prefix("//") {
                // The server is the host of the URI, and the rest is the path.
                encode(&mut uri, share);
            } else {
                encode(&mut uri, slashed);
            }
        }
    }
    uri
}

fn encode(uri: &mut String, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(byte));
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Flavor::{Posix, Windows};
    use super::*;

    #[test]
    fn what_is_absolute_is_what_node_says() {
        assert!(is_absolute("/home/u", Posix));
        assert!(!is_absolute("home/u", Posix));
        assert!(!is_absolute(r"C:\Users", Posix));
        assert!(!is_absolute("", Posix));

        for absolute in [
            r"C:\Users\u",
            "C:/Users/u",
            r"c:\",
            r"\\server\share\a",
            "//server/share/a",
            r"\rooted",
            "/rooted",
        ] {
            assert!(is_absolute(absolute, Windows), "{absolute}");
        }
        for relative in [
            r"src\a.lua",
            "src/a.lua",
            "a.lua",
            "",
            "C:",
            "C:a.lua",
            "1:\\a",
        ] {
            assert!(!is_absolute(relative, Windows), "{relative}");
        }
    }

    #[test]
    fn a_relative_path_is_joined_with_the_folder_and_cannot_leave_the_root() {
        assert_eq!(join_normalized("/a", "../../b", Posix), "/b");
        assert_eq!(join_normalized("/a/b", "./c/../d.lua", Posix), "/a/b/d.lua");

        assert_eq!(
            join_normalized(r"C:\Users\u\proj", r"src\a.lua", Windows),
            r"C:\Users\u\proj\src\a.lua"
        );
        // The separator is the one that the folder uses, whatever the relative path has.
        assert_eq!(
            join_normalized(r"C:\Users\u\proj", "src/../lib/a.lua", Windows),
            r"C:\Users\u\proj\lib\a.lua"
        );
        assert_eq!(
            join_normalized("C:/Users/u/proj", r"src\a.lua", Windows),
            "C:/Users/u/proj/src/a.lua"
        );
        // The drive is the root: `..` does not go above it.
        assert_eq!(join_normalized(r"C:\a", r"..\..\b", Windows), r"C:\b");
        assert_eq!(join_normalized(r"C:\a", r"..", Windows), r"C:\");
        // A share is a root, of the server and the name of the share.
        assert_eq!(
            join_normalized(r"\\server\share\dir", r"..\..\..\a.lua", Windows),
            r"\\server\share\a.lua"
        );
        // What is rooted at the separator, with no drive, stays as it is written (the paths of the tests of the core).
        assert_eq!(
            join_normalized("/home/u", "a.lua", Windows),
            "/home/u/a.lua"
        );
    }

    #[test]
    fn a_file_uri_encodes_what_is_not_safe_and_keeps_the_slashes() {
        assert_eq!(
            file_uri("/home/u/my project", Posix),
            "file:///home/u/my%20project"
        );
        assert_eq!(file_uri("/a/日本", Posix), "file:///a/%E6%97%A5%E6%9C%AC");
        // On Unix a backslash is a character of a name.
        assert_eq!(file_uri(r"/a\b", Posix), "file:///a%5Cb");
    }

    #[test]
    fn a_windows_path_is_a_file_uri_with_the_drive_after_three_slashes() {
        assert_eq!(
            file_uri(r"C:\Users\u\a b.lua", Windows),
            "file:///C:/Users/u/a%20b.lua"
        );
        // The case of the drive letter is what the editor said: the server does not change it.
        assert_eq!(
            file_uri("c:/x/日本.lua", Windows),
            "file:///c:/x/%E6%97%A5%E6%9C%AC.lua"
        );
        assert_eq!(file_uri(r"\\?\C:\x\a.lua", Windows), "file:///C:/x/a.lua");
        assert_eq!(
            file_uri(r"\\server\share\a b.lua", Windows),
            "file://server/share/a%20b.lua"
        );
        // A path with no drive is what it is.
        assert_eq!(file_uri("/home/u/a.lua", Windows), "file:///home/u/a.lua");
    }
}
