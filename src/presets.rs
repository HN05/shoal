//! Build tools a repository names in `presets`, and what each sets up so its
//! workspaces stop repeating the same work: a seed build of the default
//! branch whose outputs new workspaces clone, environment that lets a
//! tool's own cache serve every workspace, and shared caches.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, ensure};

use crate::{
    config::repo::{Cache, CacheScope},
    state::states,
};

pub mod derived_data;
pub mod xcode;

/// The shared cache the `xcode` preset keeps its files in.
pub const XCODE_CACHE: &str = "preset.xcode";

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
        /// Go: builds without workspace paths so its shared build cache
        /// serves every workspace, and seeds that cache.
        Go => "go",
        /// Xcode: one compilation cache for every workspace's `xcodebuild`.
        Xcode => "xcode",
        /// Zig: one local cache for every workspace, and seeds it.
        Zig => "zig",
        /// ccache: C and C++ builds through CMake share one compiler cache.
        Ccache => "ccache",
    }
);

impl Preset {
    /// The seed build's command, which reproduces the committed lockfile
    /// exactly and fails without one. Yarn 2 and later read
    /// `--frozen-lockfile` as `--immutable`.
    pub fn seed_command(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Cargo => Some(&["cargo", "build", "--all-targets"]),
            Self::Npm => Some(&["npm", "ci"]),
            Self::Pnpm => Some(&["pnpm", "install", "--frozen-lockfile"]),
            Self::Yarn => Some(&["yarn", "install", "--frozen-lockfile"]),
            Self::Bun => Some(&["bun", "install", "--frozen-lockfile"]),
            Self::Go => Some(&["go", "build", "./..."]),
            Self::Zig => Some(&["zig", "build"]),
            // Building needs a scheme, and Xcode's outputs record their path;
            // C builds have no usual command.
            Self::Xcode | Self::Ccache => None,
        }
    }

    /// The worktree-relative seed output new workspaces start with, if the
    /// build keeps one inside the worktree.
    pub fn seed_output(self) -> Option<&'static str> {
        match self {
            Self::Cargo => Some("target"),
            Self::Npm | Self::Pnpm | Self::Yarn | Self::Bun => Some("node_modules"),
            Self::Go | Self::Xcode | Self::Zig | Self::Ccache => None,
        }
    }

    /// Variables the preset sets where `[env]` does not, rendered like
    /// `[env]`. Go's cache keys include a package's directory unless paths
    /// are trimmed; ccache's, the compiler's working directory and paths
    /// outside its base directory.
    fn environment(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Go => &[("GOFLAGS", "-trimpath")],
            Self::Xcode => &[
                ("XCODE_XCCONFIG_FILE", "{cache.preset.xcode}/shoal.xcconfig"),
                (crate::env::PATH_PREFIX, "{cache.preset.xcode}/bin"),
            ],
            Self::Zig => &[("ZIG_LOCAL_CACHE_DIR", "{cache.preset.zig}")],
            Self::Ccache => &[
                ("CCACHE_BASEDIR", "{workspace_path}"),
                ("CCACHE_NOHASHDIR", "true"),
                ("CMAKE_C_COMPILER_LAUNCHER", "ccache"),
                ("CMAKE_CXX_COMPILER_LAUNCHER", "ccache"),
            ],
            _ => &[],
        }
    }

    /// The shared cache the preset keeps its files in. Configured cache
    /// names cannot contain a dot, so only the preset prepares it.
    fn cache(self) -> Option<&'static str> {
        match self {
            Self::Xcode => Some(XCODE_CACHE),
            Self::Zig => Some("preset.zig"),
            _ => None,
        }
    }
}

/// `caches` with each preset's shared cache.
pub fn caches(presets: &[Preset], mut caches: BTreeMap<String, Cache>) -> BTreeMap<String, Cache> {
    for name in presets.iter().filter_map(|preset| preset.cache()) {
        caches.insert(
            name.to_owned(),
            Cache {
                scope: CacheScope::Shared,
            },
        );
    }
    caches
}

/// Write the files a preset keeps in its cache `directory`, once the
/// directory exists.
pub fn prepare_cache(name: &str, directory: &Path) -> Result<()> {
    if name == XCODE_CACHE {
        xcode::prepare(directory)?;
    }
    Ok(())
}

/// `env` with every preset's variables it does not set itself.
pub fn environment(
    presets: &[Preset],
    mut env: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for (name, value) in presets.iter().flat_map(|preset| preset.environment()) {
        env.entry((*name).to_owned())
            .or_insert_with(|| (*value).to_owned());
    }
    env
}

/// Each preset is named once, and no two keep the same seed output.
pub fn validate(presets: &[Preset]) -> Result<()> {
    for (index, preset) in presets.iter().enumerate() {
        for earlier in &presets[..index] {
            ensure!(earlier != preset, "presets names {preset} twice");
            if let Some(output) = preset.seed_output() {
                ensure!(
                    earlier.seed_output() != Some(output),
                    "presets {earlier} and {preset} both keep {output}"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
