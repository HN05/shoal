//! The shared config: `shared.toml` beside the global config, with its mode
//! and rule files in `shared/` beside it, is the layer under each machine's
//! own file that the user's machines share.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Config, machine, parse, repo::RepoConfig};
use crate::paths::Paths;

/// The shared config's file name, beside the global config.
const FILE: &str = "shared.toml";

/// The directory beside the shared config that holds its `modes/` and
/// `rules/` files.
pub(super) const TEMPLATES_DIR: &str = "shared";

/// The shared config as written, once valid on its own.
#[derive(Debug, Default, Clone)]
pub struct Shared {
    pub(super) machine: machine::Written,
    pub(super) repository: RepoConfig,
}

impl Shared {
    pub fn path(paths: &Paths) -> PathBuf {
        Self::path_for_home(&paths.home)
    }

    /// The shared config beside the global config of `home`.
    pub fn path_for_home(home: &Path) -> PathBuf {
        Config::path_for_home(home).with_file_name(FILE)
    }

    /// The file on disk; none while there is no file.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = Self::path(paths);
        let text = super::read_file(&path)?;
        Self::parse(&text, paths).with_context(|| format!("parse {}", path.display()))
    }

    /// `text` as the shared config. Every machine reads it, also one whose
    /// own file sets nothing, so it must be valid over the defaults alone.
    pub fn parse(text: &str, paths: &Paths) -> Result<Self> {
        let parse::GlobalFile {
            machine,
            repository,
        } = parse::shared(text)?;
        let shared = Self {
            machine,
            repository,
        };
        Config::checked(&shared, None, paths)?;
        Ok(shared)
    }
}

#[cfg(test)]
mod tests;
