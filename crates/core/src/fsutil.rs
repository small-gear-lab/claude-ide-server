//! Small file system helpers shared by the modules that create private directories.

use std::fs;
use std::io;
use std::path::Path;

/// Creates a directory (and its parents if they are missing) that only the user can use.
///
/// On Unix that is mode 0700. On Windows the directories that are used (the profile of the user, and the temporary
/// directory) are the user's by what a new directory inherits, so the directory is made as it is.
pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}
