//! File helpers the client-side configuration reads with.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Expand a leading `~` path component, without resolving users or symlinks.
pub(crate) fn expand_home(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(relative) => home.join(relative),
        Err(_) => path.to_owned(),
    }
}

/// Read UTF-8 text; only a missing file is treated as absent.
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
