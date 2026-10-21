//! What a seed build runs and keeps: the repository's own `seed.cmd` and
//! `seed.paths`, or the usual build and outputs of each preset tool.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::config::repo::{Seed, SeedPreset};

/// The commands a seed build runs in its worktree.
#[derive(Debug, Clone, Serialize)]
// Untagged, so a script keeps the settings its earlier snapshots recorded.
#[serde(untagged)]
pub enum Build {
    Script(PathBuf),
    Presets(Vec<SeedPreset>),
}

/// One command of a seed build, and how errors and the log name it.
#[derive(Debug)]
pub struct Step {
    pub label: String,
    pub argv: Vec<OsString>,
}

impl Build {
    /// The build `seed` describes for the seed worktree, or `None` when it
    /// names neither a command nor a preset.
    pub fn of(seed: &Seed, worktree: &Path) -> Option<Self> {
        match &seed.cmd {
            Some(command) => Some(Self::Script(worktree.join(command))),
            None if seed.presets.is_empty() => None,
            None => Some(Self::Presets(seed.presets.clone())),
        }
    }

    /// The commands to run, in order.
    pub fn steps(&self) -> Vec<Step> {
        match self {
            Self::Script(command) => vec![Step {
                label: "seed.cmd".into(),
                argv: vec![command.into()],
            }],
            Self::Presets(presets) => presets.iter().map(|preset| preset.step()).collect(),
        }
    }
}

/// The outputs new workspaces start with: `seed.paths`, or else each
/// preset's.
pub fn outputs(seed: &Seed) -> Vec<String> {
    if !seed.paths.is_empty() {
        return seed.paths.clone();
    }
    seed.presets
        .iter()
        .map(|preset| preset.output().to_owned())
        .collect()
}

impl SeedPreset {
    /// The worktree-relative output new workspaces start with.
    pub fn output(self) -> &'static str {
        match self {
            Self::Cargo => "target",
            Self::Npm | Self::Pnpm | Self::Yarn | Self::Bun => "node_modules",
        }
    }

    /// The build, which reproduces the committed lockfile exactly and fails
    /// without one. Yarn 2 and later read `--frozen-lockfile` as `--immutable`.
    fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Cargo => &["cargo", "build", "--all-targets"],
            Self::Npm => &["npm", "ci"],
            Self::Pnpm => &["pnpm", "install", "--frozen-lockfile"],
            Self::Yarn => &["yarn", "install", "--frozen-lockfile"],
            Self::Bun => &["bun", "install", "--frozen-lockfile"],
        }
    }

    fn step(self) -> Step {
        let argv = self.argv();
        Step {
            label: argv.join(" "),
            argv: argv.iter().map(OsString::from).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(text: &str) -> Seed {
        crate::config::repo::parse(text).unwrap().seed
    }

    fn labels(build: &Build) -> Vec<String> {
        build.steps().into_iter().map(|step| step.label).collect()
    }

    #[test]
    fn presets_build_and_keep_what_their_tools_use() {
        let worktree = Path::new("/seed/worktree");
        let presets = seed("[seed]\npresets = ['cargo', 'bun']\n");
        let build = Build::of(&presets, worktree).unwrap();
        assert_eq!(
            labels(&build),
            ["cargo build --all-targets", "bun install --frozen-lockfile"]
        );
        assert_eq!(outputs(&presets), ["target", "node_modules"]);

        // A command and paths of the repository's own replace the presets'.
        let own = seed("[seed]\npresets = ['cargo']\ncmd = 'seed.sh'\npaths = ['out']\n");
        let build = Build::of(&own, worktree).unwrap();
        let steps = build.steps();
        assert_eq!(steps[0].label, "seed.cmd");
        assert_eq!(steps[0].argv, [worktree.join("seed.sh").into_os_string()]);
        assert_eq!(outputs(&own), ["out"]);
        assert!(Build::of(&seed(""), worktree).is_none());
    }

    #[test]
    fn presets_are_named_once_and_keep_distinct_outputs() {
        for (text, expected) in [
            ("[seed]\npresets = ['npm', 'npm']\n", "twice"),
            (
                "[seed]\npresets = ['npm', 'pnpm']\n",
                "both keep node_modules",
            ),
        ] {
            let error = crate::config::repo::parse(text).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    #[test]
    fn a_script_keeps_the_settings_of_earlier_snapshots() {
        let build = Build::Script("/seed/worktree/seed.sh".into());
        assert_eq!(
            serde_json::to_string(&build).unwrap(),
            serde_json::to_string(Path::new("/seed/worktree/seed.sh")).unwrap()
        );
    }
}
