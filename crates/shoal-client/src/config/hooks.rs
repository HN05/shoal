//! The hooks a repository configures: their config keys, and the directory
//! each runs in.
use std::path::Path;

use anyhow::{Result, ensure};

use super::{Settings, repo::RepoConfig};

/// The directory used both to resolve a hook's executable and to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDirectory {
    Worktree,
    Checkout,
}

impl HookDirectory {
    pub fn path<'a>(self, worktree: &'a Path, checkout: &'a Path) -> &'a Path {
        match self {
            Self::Worktree => worktree,
            Self::Checkout => checkout,
        }
    }
}

// Keep the config field, environment name and directory in one table. Config
// structs retain their existing TOML keys and strict parsing.
macro_rules! hook_kinds {
    ($($kind:ident => ($($field:ident).+, $name:literal, $directory:ident)),+ $(,)?) => {
        crate::state::states!(HookKind { $($kind => $name),+ });

        impl HookKind {
            pub const ALL: &[Self] = &[$(Self::$kind),+];

            pub fn key(self) -> &'static str {
                match self { $(Self::$kind => stringify!($($field).+)),+ }
            }

            pub fn directory(self) -> HookDirectory {
                match self { $(Self::$kind => HookDirectory::$directory),+ }
            }

            pub fn repository_command(self, config: &RepoConfig) -> Option<&String> {
                match self { $(Self::$kind => config.$($field).+.as_ref()),+ }
            }

            pub fn repository_command_mut(self, config: &mut RepoConfig) -> Option<&mut String> {
                match self { $(Self::$kind => config.$($field).+.as_mut()),+ }
            }

            pub fn command(self, settings: &Settings) -> Option<&String> {
                match self { $(Self::$kind => settings.$($field).+.as_ref()),+ }
            }

            pub fn validate(self, command: Option<&String>) -> Result<()> {
                if let Some(command) = command {
                    ensure!(
                        !command.trim().is_empty() && !command.contains('\0'),
                        "{} must be a nonempty executable path", self.key()
                    );
                }
                Ok(())
            }
        }
    };
}

hook_kinds! {
    Setup => (setup.command, "setup", Worktree),
    PreSetup => (hooks.pre_setup, "pre_setup", Worktree),
    PostSetup => (hooks.post_setup, "post_setup", Worktree),
    PreRemove => (hooks.pre_remove, "pre_remove", Worktree),
    PostRemove => (hooks.post_remove, "post_remove", Checkout),
    PostDone => (hooks.post_done, "post_done", Worktree),
    PostReady => (hooks.post_ready, "post_ready", Worktree),
    PostAgentExit => (hooks.post_agent_exit, "post_agent_exit", Worktree),
    PostResourceAcquire => (hooks.post_resource_acquire, "post_resource_acquire", Worktree),
    PreResourceRelease => (hooks.pre_resource_release, "pre_resource_release", Worktree),
}
