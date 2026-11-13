//! Filesystem operations shared by the CLI and daemon, a module for each
//! responsibility.
pub use crate::client::executions::FileIdentity;
use std::{fs, io, path::Path};

pub mod atomic;
pub mod copy;
pub mod current_dir;
pub(crate) mod descriptors;
pub mod disk_space;
pub mod executables;
pub mod home;
pub mod marked_dirs;
pub mod off_runtime;
pub mod subvolume;

/// Read UTF-8 text; only a missing file is treated as absent.
pub fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) mod tests;
