//! What a seed build runs and keeps: the repository's own `seed.cmd` and
//! `seed.paths`, or the usual build and outputs of each preset tool.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
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

    /// The commands to run, in order, for the commit checked out in `worktree`.
    pub fn steps(&self, worktree: &Path) -> Result<Vec<Step>> {
        match self {
            Self::Script(command) => Ok(vec![Step {
                label: "seed.cmd".into(),
                argv: vec![command.into()],
            }]),
            Self::Presets(presets) => presets
                .iter()
                .map(|preset| preset_step(*preset, worktree))
                .collect(),
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
        .map(|preset| output(*preset).to_owned())
        .collect()
}

fn output(preset: SeedPreset) -> &'static str {
    match preset {
        SeedPreset::Cargo => "target",
        SeedPreset::Npm => "node_modules",
    }
}

fn preset_step(preset: SeedPreset, worktree: &Path) -> Result<Step> {
    let argv: &[&str] = match preset {
        SeedPreset::Cargo => &["cargo", "build", "--all-targets"],
        SeedPreset::Npm => node_install(worktree)?,
    };
    Ok(Step {
        label: argv.join(" "),
        argv: argv.iter().map(OsString::from).collect(),
    })
}

/// The install that reproduces the committed lockfile exactly.
fn node_install(worktree: &Path) -> Result<&'static [&'static str]> {
    if worktree.join("pnpm-lock.yaml").is_file() {
        return Ok(&["pnpm", "install", "--frozen-lockfile"]);
    }
    if ["package-lock.json", "npm-shrinkwrap.json"]
        .iter()
        .any(|lockfile| worktree.join(lockfile).is_file())
    {
        return Ok(&["npm", "ci"]);
    }
    bail!("the npm seed preset needs a committed package-lock.json or pnpm-lock.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn seed(text: &str) -> Seed {
        crate::config::repo::parse(text).unwrap().seed
    }

    fn labels(build: &Build, worktree: &Path) -> Vec<String> {
        build
            .steps(worktree)
            .unwrap()
            .into_iter()
            .map(|step| step.label)
            .collect()
    }

    #[test]
    fn presets_build_and_keep_what_their_tools_use() {
        let worktree = tempfile::tempdir().unwrap();
        fs::write(worktree.path().join("pnpm-lock.yaml"), "").unwrap();
        let presets = seed("[seed]\npresets = ['cargo', 'npm']\n");
        let build = Build::of(&presets, worktree.path()).unwrap();
        assert_eq!(
            labels(&build, worktree.path()),
            [
                "cargo build --all-targets",
                "pnpm install --frozen-lockfile"
            ]
        );
        assert_eq!(outputs(&presets), ["target", "node_modules"]);

        // A command and paths of the repository's own replace the presets'.
        let own = seed("[seed]\npresets = ['cargo']\ncmd = 'seed.sh'\npaths = ['out']\n");
        let build = Build::of(&own, worktree.path()).unwrap();
        assert_eq!(labels(&build, worktree.path()), ["seed.cmd"]);
        assert_eq!(
            build.steps(worktree.path()).unwrap()[0].argv,
            [worktree.path().join("seed.sh").into_os_string()]
        );
        assert_eq!(outputs(&own), ["out"]);
        assert!(Build::of(&seed(""), worktree.path()).is_none());
    }

    #[test]
    fn the_npm_preset_follows_the_committed_lockfile() {
        let worktree = tempfile::tempdir().unwrap();
        let build = Build::Presets(vec![SeedPreset::Npm]);
        let error = build.steps(worktree.path()).unwrap_err();
        assert!(error.to_string().contains("package-lock.json"), "{error}");
        fs::write(worktree.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(labels(&build, worktree.path()), ["npm ci"]);
    }

    #[test]
    fn a_preset_is_named_once() {
        let error = crate::config::repo::parse("[seed]\npresets = ['npm', 'npm']\n").unwrap_err();
        assert!(format!("{error:#}").contains("twice"), "{error:#}");
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
