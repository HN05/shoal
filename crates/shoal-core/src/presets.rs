//! What build tool presets keep on disk: the files of their shared caches
//! and Xcode's derived data. [`crate::config::presets`] names the presets.
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use anyhow::{Context as _, Result};

pub(crate) use crate::config::presets::{CARGO_CACHE, KEPT_CACHES, XCODE_FILES_CACHE};
pub use crate::config::presets::{Preset, XCODE_CACHE};

pub(crate) mod cargo;
pub mod derived_data;
pub mod xcode;

/// Write the files a preset keeps in its cache `directory`, once the
/// directory exists. A preset's caches lie side by side.
pub(crate) fn prepare_cache(name: &str, directory: &Path) -> Result<()> {
    match name {
        CARGO_CACHE => cargo::prepare(directory),
        XCODE_FILES_CACHE => xcode::prepare(directory, &directory.with_file_name(XCODE_CACHE)),
        _ => Ok(()),
    }
}

/// Write `content` to `path` with `mode`, leaving a file whose content is
/// current untouched, since build tools compare its modification time.
fn write_if_changed(path: &Path, content: &str, mode: u32) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == content) {
        return Ok(());
    }
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set permissions on {}", path.display()))
}

#[cfg(test)]
mod tests;
