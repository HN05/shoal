//! What build tool presets keep on disk: the files of their shared caches
//! and Xcode's derived data. [`crate::config::presets`] names the presets.
use std::path::Path;

use anyhow::Result;

pub(crate) use crate::config::presets::{Preset, XCODE_CACHE};

pub mod derived_data;
pub mod xcode;

/// Write the files a preset keeps in its cache `directory`, once the
/// directory exists.
pub fn prepare_cache(name: &str, directory: &Path) -> Result<()> {
    if name == XCODE_CACHE {
        xcode::prepare(directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
