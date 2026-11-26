//! The shared config: `shared.toml` beside the global config, with its mode
//! and rule files in `shared/` beside it, is the layer under each machine's
//! own file that the user's machines share.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use shoal_protocol::hub::shared_config::FileName;

use super::{Config, machine, modes, parse, repo::RepoConfig};
use crate::paths::Paths;

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

    pub fn file(paths: &Paths, name: &FileName) -> PathBuf {
        match name {
            FileName::Config => Self::path(paths),
            name => Self::path(paths)
                .with_file_name(TEMPLATES_DIR)
                .join(name.to_string()),
        }
    }

    /// The text of each shared file on disk, by name. A file whose name no
    /// mode or rule may have is left out, as loading the config refuses it.
    pub fn files(paths: &Paths) -> Result<BTreeMap<FileName, String>> {
        let directory = Self::path(paths).with_file_name(TEMPLATES_DIR);
        let mut names = vec![FileName::Config];
        for kind in [modes::MODES_DIR, modes::RULES_DIR] {
            names.extend(names_in(&directory.join(kind), kind)?);
        }
        let mut files = BTreeMap::new();
        for name in names {
            if let Some(text) = read_optional(&Self::file(paths, &name))? {
                files.insert(name, text);
            }
        }
        Ok(files)
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

/// The names of the shared files in `directory`, the shared config's
/// `kind` subdirectory.
fn names_in(directory: &Path, kind: &str) -> Result<Vec<FileName>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", directory.display()))?;
        let name = entry.file_name();
        let name = name.to_str().map(|name| format!("{kind}/{name}").parse());
        if let Some(Ok(name)) = name {
            names.push(name);
        }
    }
    Ok(names)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    crate::fsutil::read_optional(path).with_context(|| format!("read {}", path.display()))
}

#[cfg(test)]
mod tests;
