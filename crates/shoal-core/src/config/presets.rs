//! Build tools a repository names in `presets`, and what each sets up. See
//! `docs/design/repositories.md`.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use shoal_states::states;

use crate::config::repo::{Cache, CacheScope};

pub(crate) const CARGO_CACHE: &str = "preset.cargo";
pub(crate) const XCODE_FILES_CACHE: &str = "preset.xcode-files";
pub const XCODE_CACHE: &str = "preset.xcode";
/// The preset caches that hold only a preset's own files, which emptying
/// caches keeps since it frees nothing there.
pub(crate) const KEPT_CACHES: &[&str] = &[CARGO_CACHE, XCODE_FILES_CACHE];

states!(
    Preset {
        Cargo => "cargo",
        Npm => "npm",
        Pnpm => "pnpm",
        Yarn => "yarn",
        Bun => "bun",
        Go => "go",
        Xcode => "xcode",
        Zig => "zig",
        Ccache => "ccache",
    }
);

/// One command of a preset's seed build.
#[derive(Debug, Clone, Copy)]
pub enum SeedCommand {
    /// A command whose non-zero exit fails the build.
    Required(&'static [&'static str]),
    /// A command after whose non-zero exit the build goes on, and keeps the
    /// outputs of its other commands.
    Soft(&'static [&'static str]),
}

impl SeedCommand {
    pub fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Required(argv) | Self::Soft(argv) => argv,
        }
    }

    pub fn is_soft(self) -> bool {
        matches!(self, Self::Soft(_))
    }
}

impl Preset {
    /// The seed build's commands, in order, or none when the preset has no
    /// usual build. The package managers' commands reproduce the committed
    /// lockfile exactly and fail without one; Yarn 2 and later read
    /// `--frozen-lockfile` as `--immutable`.
    pub fn seed_commands(self) -> &'static [SeedCommand] {
        use SeedCommand::{Required, Soft};
        match self {
            Self::Cargo => &[
                Required(&["cargo", "build", "--all-targets"]),
                Required(&["cargo", "test", "--no-run"]),
                Required(&["cargo", "check", "--all-targets"]),
                // Its outputs serve only clippy runs, and clippy is an
                // optional component, which a toolchain can lack.
                Soft(&["cargo", "clippy", "--all-targets"]),
            ],
            Self::Npm => &[Required(&["npm", "ci"])],
            Self::Pnpm => &[Required(&["pnpm", "install", "--frozen-lockfile"])],
            Self::Yarn => &[Required(&["yarn", "install", "--frozen-lockfile"])],
            Self::Bun => &[Required(&["bun", "install", "--frozen-lockfile"])],
            Self::Go => &[Required(&["go", "build", "./..."])],
            Self::Zig => &[Required(&["zig", "build"])],
            // Building needs a scheme, and Xcode's outputs record their path;
            // C builds have no usual command.
            Self::Xcode | Self::Ccache => &[],
        }
    }

    /// The command that deletes what the seed build made of the repository's
    /// own code, run before a build whose outputs would otherwise keep
    /// earlier builds beside it. Cargo never deletes a unit whose hash
    /// changed; its clean keeps third-party units.
    pub fn reset_command(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Cargo => Some(&["cargo", "clean", "--workspace"]),
            _ => None,
        }
    }

    /// The command whose output names the toolchain the seed builds with.
    pub fn toolchain_command(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Cargo => Some(&["rustc", "-vV"]),
            _ => None,
        }
    }

    /// The worktree-relative files whose change makes the seed build run
    /// [`Self::reset_command`] first; a leading `**/` names the file in any
    /// directory.
    pub fn reset_files(self) -> &'static [&'static str] {
        match self {
            Self::Cargo => &[
                "Cargo.lock",
                "**/Cargo.toml",
                "rust-toolchain",
                "rust-toolchain.toml",
                ".cargo/config.toml",
            ],
            _ => &[],
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
            Self::Cargo => &[
                (
                    "RUSTC_WORKSPACE_WRAPPER",
                    "{cache.preset.cargo}/rustc-workspace-wrapper",
                ),
                ("RUSTC_WRAPPER", "{cache.preset.cargo}/rustc-clippy-wrapper"),
            ],
            Self::Go => &[("GOFLAGS", "-trimpath")],
            Self::Xcode => &[
                (
                    "XCODE_XCCONFIG_FILE",
                    "{cache.preset.xcode-files}/shoal.xcconfig",
                ),
                (crate::env::PATH_PREFIX, "{cache.preset.xcode-files}/bin"),
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

    /// Variables the preset gives CI jobs on Shoal's runner. Cargo builds
    /// without incremental compilation under `CI`, which runners set, and
    /// would then rebuild every crate the seed built incrementally.
    fn job_environment(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Cargo => &[("CARGO_INCREMENTAL", "1")],
            _ => &[],
        }
    }

    /// The MCP servers the preset brings, each by its name and command
    /// line, which `[mcp]` changes.
    pub(crate) fn mcp_servers(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Self::Xcode => &[("xcode", &["xcrun", "mcpbridge"])],
            _ => &[],
        }
    }

    /// The shared caches the preset keeps its files and data in. Configured
    /// cache names cannot contain a dot, so only the preset prepares them.
    fn caches(self) -> &'static [&'static str] {
        match self {
            Self::Cargo => &[CARGO_CACHE],
            Self::Xcode => &[XCODE_FILES_CACHE, XCODE_CACHE],
            Self::Zig => &["preset.zig"],
            _ => &[],
        }
    }
}

/// `caches` with each preset's shared caches.
pub(crate) fn caches(
    presets: &[Preset],
    mut caches: BTreeMap<String, Cache>,
) -> BTreeMap<String, Cache> {
    for name in presets
        .iter()
        .flat_map(|preset| preset.caches().iter().copied())
    {
        caches.insert(
            name.to_owned(),
            Cache {
                scope: CacheScope::Shared,
            },
        );
    }
    caches
}

/// `env` with every preset's variables it does not set itself.
pub(crate) fn environment(
    presets: &[Preset],
    mut env: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for (name, value) in presets.iter().flat_map(|preset| preset.environment()) {
        env.entry((*name).to_owned())
            .or_insert_with(|| (*value).to_owned());
    }
    env
}

/// The variables `presets` give CI jobs that `env`, the resolved `[env]`,
/// does not set.
pub fn job_environment(
    presets: &[Preset],
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    presets
        .iter()
        .flat_map(|preset| preset.job_environment())
        .filter(|(name, _)| !env.contains_key(*name))
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// Each preset is named once, and no two keep the same seed output.
pub(crate) fn validate(presets: &[Preset]) -> Result<()> {
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
