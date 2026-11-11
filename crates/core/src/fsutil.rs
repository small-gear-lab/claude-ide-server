//! Small file system helpers shared by the modules that create private directories.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

/// Creates a directory (and its parents if they are missing) with mode 0700.
pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}
