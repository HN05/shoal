//! What a seed build runs and keeps: the repository's own `seed.cmd` and
//! `seed.paths`, or the usual build and outputs of each preset tool.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{config::repo::Seed, presets::Preset};

/// The commands a seed build runs in its worktree.
#[derive(Debug, Clone, Serialize)]
// Untagged, so a script keeps the settings its earlier snapshots recorded.
#[serde(untagged)]
pub enum Build {
    Script(PathBuf),
    Presets(Vec<Preset>),
}

/// One command of a seed build, and how errors and the log name it.
#[derive(Debug)]
pub struct Step {
    pub label: String,
    pub argv: Vec<OsString>,
}

impl Build {
    /// The build `seed` and `presets` describe for the seed worktree, or
    /// `None` when they name neither a command nor a preset.
    pub fn of(seed: &Seed, presets: &[Preset], worktree: &Path) -> Option<Self> {
        let building: Vec<_> = presets
            .iter()
            .copied()
            .filter(|preset| preset.seed_command().is_some())
            .collect();
        match &seed.cmd {
            Some(command) => Some(Self::Script(worktree.join(command))),
            None if building.is_empty() => None,
            None => Some(Self::Presets(building)),
        }
    }

    /// The commands to run, in order.
    pub fn steps(&self) -> Vec<Step> {
        match self {
            Self::Script(command) => vec![Step {
                label: "seed.cmd".into(),
                argv: vec![command.into()],
            }],
            Self::Presets(presets) => presets.iter().filter_map(|preset| step(*preset)).collect(),
        }
    }
}

/// The outputs new workspaces start with: `seed.paths`, or else each
/// preset's.
pub fn outputs(seed: &Seed, presets: &[Preset]) -> Vec<String> {
    if !seed.paths.is_empty() {
        return seed.paths.clone();
    }
    presets
        .iter()
        .filter_map(|preset| preset.seed_output().map(str::to_owned))
        .collect()
}

fn step(preset: Preset) -> Option<Step> {
    let argv = preset.seed_command()?;
    Some(Step {
        label: argv.join(" "),
        argv: argv.iter().map(OsString::from).collect(),
    })
}

#[cfg(test)]
mod tests;
