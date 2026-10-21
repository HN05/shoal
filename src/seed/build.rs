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
        match &seed.cmd {
            Some(command) => Some(Self::Script(worktree.join(command))),
            None if presets.is_empty() => None,
            None => Some(Self::Presets(presets.to_vec())),
        }
    }

    /// The commands to run, in order.
    pub fn steps(&self) -> Vec<Step> {
        match self {
            Self::Script(command) => vec![Step {
                label: "seed.cmd".into(),
                argv: vec![command.into()],
            }],
            Self::Presets(presets) => presets.iter().map(|preset| step(*preset)).collect(),
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
        .map(|preset| preset.seed_output().to_owned())
        .collect()
}

fn step(preset: Preset) -> Step {
    let argv = preset.seed_command();
    Step {
        label: argv.join(" "),
        argv: argv.iter().map(OsString::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::repo::{RepoConfig, parse};

    fn config(text: &str) -> RepoConfig {
        parse(text).unwrap()
    }

    fn labels(build: &Build) -> Vec<String> {
        build.steps().into_iter().map(|step| step.label).collect()
    }

    #[test]
    fn presets_build_and_keep_what_their_tools_use() {
        let worktree = Path::new("/seed/worktree");
        let presets = config("presets = ['cargo', 'bun']\n");
        let build = Build::of(&presets.seed, &presets.presets, worktree).unwrap();
        assert_eq!(
            labels(&build),
            ["cargo build --all-targets", "bun install --frozen-lockfile"]
        );
        assert_eq!(
            outputs(&presets.seed, &presets.presets),
            ["target", "node_modules"]
        );

        // A command and paths of the repository's own replace the presets'.
        let own = config("presets = ['cargo']\n[seed]\ncmd = 'seed.sh'\npaths = ['out']\n");
        let build = Build::of(&own.seed, &own.presets, worktree).unwrap();
        let steps = build.steps();
        assert_eq!(steps[0].label, "seed.cmd");
        assert_eq!(steps[0].argv, [worktree.join("seed.sh").into_os_string()]);
        assert_eq!(outputs(&own.seed, &own.presets), ["out"]);
        let none = config("");
        assert!(Build::of(&none.seed, &none.presets, worktree).is_none());
    }

    #[test]
    fn presets_are_named_once_and_keep_distinct_outputs() {
        for (text, expected) in [
            ("presets = ['npm', 'npm']\n", "twice"),
            ("presets = ['npm', 'pnpm']\n", "both keep node_modules"),
        ] {
            let error = parse(text).unwrap_err();
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
