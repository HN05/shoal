//! What build tool presets keep on disk: the files of their shared caches
//! and Xcode's derived data. [`crate::config::presets`] names the presets.
use std::{fs, path::Path};

use anyhow::{Context as _, Result};

use crate::fsutil::atomic::{Permissions, ReplaceOptions, replace_atomically};

pub(crate) use crate::config::presets::{CARGO_CACHE, KEPT_CACHES, XCODE_FILES_CACHE};
pub use crate::config::presets::{Preset, SeedCommand, XCODE_CACHE};

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
/// current untouched, since build tools compare its modification time. The
/// file appears whole or not at all, so a full disk never leaves a
/// truncated or unexecutable script for a running tool to find.
fn write_if_changed(path: &Path, content: &str, mode: u32) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == content) {
        return Ok(());
    }
    let options = ReplaceOptions {
        permissions: Permissions::Mode(mode),
        sync: false,
    };
    replace_atomically(path, content.as_bytes(), options)
        .with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests;
