//! The command a config sets for each hook.
pub use crate::client::config::hooks::*;

use super::{Settings, repo::RepoConfig};

/// Where a config sets each hook's command.
pub trait HookCommand: Sized {
    /// The command a repository layer sets.
    fn repository_command(self, config: &RepoConfig) -> Option<&String>;

    fn repository_command_mut(self, config: &mut RepoConfig) -> Option<&mut String>;

    /// The command the resolved settings run.
    fn command(self, settings: &Settings) -> Option<&String>;
}

// Each hook's field, at the key [`HookKind::key`] names.
macro_rules! hook_fields {
    ($($kind:ident => $($field:ident).+),+ $(,)?) => {
        impl HookCommand for HookKind {
            fn repository_command(self, config: &RepoConfig) -> Option<&String> {
                match self { $(Self::$kind => config.$($field).+.as_ref()),+ }
            }

            fn repository_command_mut(self, config: &mut RepoConfig) -> Option<&mut String> {
                match self { $(Self::$kind => config.$($field).+.as_mut()),+ }
            }

            fn command(self, settings: &Settings) -> Option<&String> {
                match self { $(Self::$kind => settings.$($field).+.as_ref()),+ }
            }
        }

        #[cfg(test)]
        const FIELD_KEYS: &[(HookKind, &str)] = &[$((HookKind::$kind, stringify!($($field).+))),+];
    };
}

hook_fields! {
    Setup => setup.command,
    PreSetup => hooks.pre_setup,
    PostSetup => hooks.post_setup,
    PreRemove => hooks.pre_remove,
    PostRemove => hooks.post_remove,
    PostDone => hooks.post_done,
    PostReady => hooks.post_ready,
    PostAgentExit => hooks.post_agent_exit,
    PostResourceAcquire => hooks.post_resource_acquire,
    PreResourceRelease => hooks.pre_resource_release,
}

#[cfg(test)]
mod tests;
