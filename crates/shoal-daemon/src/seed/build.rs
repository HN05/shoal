//! What a seed build runs and keeps: the repository's own `setup.seed.command` and
//! `seed.paths`, or the usual build and outputs of each preset tool, which
//! first resets the outputs an earlier build left when what they depend on
//! changed.
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{config::repo::Seed, presets::Preset};

/// The output of each preset's toolchain command, as a build ran it.
pub type Toolchains = HashMap<Preset, String>;

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

/// What the current snapshot was built from, as the next build compares it.
pub struct Previous {
    pub settings: String,
    pub toolchains: Toolchains,
    /// The tracked files that changed since its commit, or `None` when the
    /// repository no longer has that commit.
    pub changed: Option<HashSet<String>>,
}

impl Build {
    /// The build `seed` and `presets` describe for the seed worktree, or
    /// `None` when they name neither a command nor a preset.
    pub fn of(seed: &Seed, presets: &[Preset], worktree: &Path) -> Option<Self> {
        let building: Vec<_> = presets
            .iter()
            .copied()
            .filter(|preset| !preset.seed_commands().is_empty())
            .collect();
        match &seed.command {
            Some(command) => Some(Self::Script(worktree.join(command))),
            None if building.is_empty() => None,
            None => Some(Self::Presets(building)),
        }
    }

    /// The commands that name each preset's toolchain.
    pub fn toolchain_steps(&self) -> Vec<(Preset, Step)> {
        self.presets()
            .iter()
            .filter_map(|preset| Some((*preset, step(preset.toolchain_command()?))))
            .collect()
    }

    /// The presets whose outputs the build resets first: every one that has
    /// a reset when no snapshot exists, else those whose outputs `previous`
    /// built under other settings or another toolchain than `toolchains`,
    /// or before a change to their reset files.
    pub fn resets(
        &self,
        previous: Option<&Previous>,
        settings: &str,
        toolchains: &Toolchains,
    ) -> Vec<Preset> {
        self.presets()
            .iter()
            .copied()
            .filter(|preset| preset.reset_command().is_some())
            .filter(|preset| {
                previous.is_none_or(|previous| previous.outdates(*preset, settings, toolchains))
            })
            .collect()
    }

    /// The commands to run, in order: the reset of each of `resets`, then
    /// the build's own.
    pub fn steps(&self, resets: &[Preset]) -> Vec<Step> {
        match self {
            Self::Script(command) => vec![Step {
                label: "setup.seed.command".into(),
                argv: vec![command.into()],
            }],
            Self::Presets(presets) => (resets.iter())
                .filter_map(|preset| preset.reset_command())
                .chain(
                    presets
                        .iter()
                        .flat_map(|preset| preset.seed_commands().iter().copied()),
                )
                .map(step)
                .collect(),
        }
    }

    fn presets(&self) -> &[Preset] {
        match self {
            Self::Script(_) => &[],
            Self::Presets(presets) => presets,
        }
    }
}

impl Previous {
    /// Whether `preset`'s outputs from this build depend on something that
    /// differs in a build under `settings` and `toolchains`.
    fn outdates(&self, preset: Preset, settings: &str, toolchains: &Toolchains) -> bool {
        self.settings != settings
            || self.toolchains.get(&preset) != toolchains.get(&preset)
            || self.changed.as_ref().is_none_or(|changed| {
                changed
                    .iter()
                    .any(|file| (preset.reset_files().iter()).any(|pattern| names(pattern, file)))
            })
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

/// Whether the worktree-relative `pattern`, which names a file in any
/// directory after a leading `**/`, names `file` as Git lists it.
fn names(pattern: &str, file: &str) -> bool {
    match pattern.strip_prefix("**/") {
        Some(name) => file
            .rsplit_once('/')
            .map_or(file, |(_, last)| last)
            .eq(name),
        None => file == pattern,
    }
}

fn step(argv: &[&str]) -> Step {
    Step {
        label: argv.join(" "),
        argv: argv.iter().map(OsString::from).collect(),
    }
}

#[cfg(test)]
mod tests;
