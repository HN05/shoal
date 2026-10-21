//! Build tools a repository names in `presets`, and what each sets up so its
//! workspaces stop repeating the same work: a seed build of the default
//! branch whose outputs new workspaces clone.
use anyhow::{Result, ensure};

use crate::state::states;

states!(
    /// A build tool whose usual setup Shoal knows.
    Preset {
        /// Cargo: seeds `target`.
        Cargo => "cargo",
        /// npm: seeds `node_modules`.
        Npm => "npm",
        /// pnpm: seeds `node_modules`.
        Pnpm => "pnpm",
        /// Yarn: seeds `node_modules`.
        Yarn => "yarn",
        /// Bun: seeds `node_modules`.
        Bun => "bun",
    }
);

impl Preset {
    /// The seed build's command, which reproduces the committed lockfile
    /// exactly and fails without one. Yarn 2 and later read
    /// `--frozen-lockfile` as `--immutable`.
    pub fn seed_command(self) -> &'static [&'static str] {
        match self {
            Self::Cargo => &["cargo", "build", "--all-targets"],
            Self::Npm => &["npm", "ci"],
            Self::Pnpm => &["pnpm", "install", "--frozen-lockfile"],
            Self::Yarn => &["yarn", "install", "--frozen-lockfile"],
            Self::Bun => &["bun", "install", "--frozen-lockfile"],
        }
    }

    /// The worktree-relative seed output new workspaces start with.
    pub fn seed_output(self) -> &'static str {
        match self {
            Self::Cargo => "target",
            Self::Npm | Self::Pnpm | Self::Yarn | Self::Bun => "node_modules",
        }
    }
}

/// Each preset is named once, and no two keep the same seed output.
pub fn validate(presets: &[Preset]) -> Result<()> {
    for (index, preset) in presets.iter().enumerate() {
        for earlier in &presets[..index] {
            ensure!(earlier != preset, "presets names {preset} twice");
            ensure!(
                earlier.seed_output() != preset.seed_output(),
                "presets {earlier} and {preset} both keep {}",
                preset.seed_output()
            );
        }
    }
    Ok(())
}
