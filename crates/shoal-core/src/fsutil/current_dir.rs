//! The directory a process runs in.
use std::{io, path::PathBuf};

/// The current directory with its links resolved, or none when it no
/// longer exists.
pub fn current_directory() -> io::Result<Option<PathBuf>> {
    match std::env::current_dir().and_then(std::fs::canonicalize) {
        Ok(cwd) => Ok(Some(cwd)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
