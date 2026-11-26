//! The `[setup]` section: preparing a workspace and seeding its outputs.
use crate::config::options::Options;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Setup)]
pub struct SetupConfig {
    pub command: Option<String>,
    #[option(default = true)]
    pub copy_ignored: Option<bool>,
    #[option(default = Vec::new())]
    pub copy_ignored_exclude: Option<Vec<String>>,
    /// Build tools whose usual setup the repository's workspaces share; an
    /// empty list turns off presets a lower layer names. Unset when no layer
    /// names presets.
    pub presets: Option<Vec<crate::config::presets::Preset>>,
    #[option(section)]
    pub seed: Seed,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Self)]
pub struct Seed {
    pub command: Option<String>,
    #[option(list)]
    pub paths: Vec<String>,
}

impl Seed {
    pub(super) fn validate(&self) -> Result<()> {
        // The seed runs unattended from the default branch, so its command
        // is the repository's own file, like its outputs.
        if let Some(command) = &self.command {
            ensure!(
                inside_worktree(command),
                "setup.seed.command must be a relative path inside the worktree: {command}"
            );
        }
        for (index, path) in self.paths.iter().enumerate() {
            ensure!(
                inside_worktree(path),
                "setup.seed.paths entries must be relative paths inside the worktree: {path}"
            );
            // Each output is cloned whole, so one cannot contain another.
            for other in &self.paths[..index] {
                ensure!(
                    !Path::new(path).starts_with(other) && !Path::new(other).starts_with(path),
                    "setup.seed.paths entries overlap: {other} and {path}"
                );
            }
        }
        Ok(())
    }
}

pub(super) fn inside_worktree(path: &str) -> bool {
    !path.trim().is_empty()
        && !path.contains('\0')
        && Path::new(path).components().all(
            |component| matches!(component, std::path::Component::Normal(name) if name != ".git"),
        )
}
