//! `[permissions]` and `[modes.NAME.permissions]`: what a scoped agent may do
//! at once, after approval, or not at all. Each value is ordered from the
//! narrowest to the widest, `deny` < `propose` < `direct`, so a space below
//! another never gets more than its parent.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        Settings, modes,
        options::{Merge, Options},
    },
    model::SpaceKind,
    state::states,
};

states!(
    #[derive(PartialOrd, Ord)]
    /// Whether a scoped caller's action happens at once or waits for
    /// approval.
    Opening {
        Propose => "propose",
        Direct => "direct",
    }
);

states!(
    #[derive(PartialOrd, Ord)]
    /// Whether a scoped caller's push happens at once, waits for approval,
    /// or is refused.
    Pushes {
        Deny => "deny",
        Propose => "propose",
        Direct => "direct",
    }
);

states!(
    #[derive(PartialOrd, Ord)]
    /// Whether a scoped caller may open an issue in another repository than
    /// its space's, at once or after approval.
    OtherRepositories {
        Deny => "deny",
        Propose => "propose",
        Direct => "direct",
    }
);

states!(
    #[derive(PartialOrd, Ord)]
    /// Whether a scoped caller may send forge API requests that are not reads.
    ApiWrites {
        Deny => "deny",
        Direct => "direct",
    }
);

/// What a scoped agent may do at once, after approval, or not at all, as
/// one layer writes it; [`resolve`] fills what no layer sets.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Self)]
pub struct PermissionsConfig {
    pub issue_open: Option<Opening>,
    pub pr_open: Option<Opening>,
    /// Opening an issue in another repository than the space's.
    pub issue_other_repositories: Option<OtherRepositories>,
    /// Merging a branch with `shoal merge`.
    pub merge: Option<Opening>,
    /// Pushing to the workspace repository's remotes, with `shoal pr open`
    /// or plain `git push`.
    pub push: Option<Pushes>,
    /// Forge API requests through `shoal forge api` that are not reads.
    pub api_writes: Option<ApiWrites>,
}

impl PermissionsConfig {
    /// Each key's value where no mode or config sets one.
    const KEY_DEFAULTS: Self = Self {
        issue_open: Some(Opening::Direct),
        pr_open: Some(Opening::Direct),
        issue_other_repositories: Some(OtherRepositories::Propose),
        merge: Some(Opening::Propose),
        push: Some(Pushes::Direct),
        api_writes: Some(ApiWrites::Deny),
    };

    /// The defaults of a mode that coordinates rather than edits: it decides
    /// its children's merges and issues elsewhere.
    pub(crate) const COORDINATING: Self = Self {
        issue_other_repositories: Some(OtherRepositories::Direct),
        merge: Some(Opening::Direct),
        ..Self::KEY_DEFAULTS
    };
}

states!(
    /// Where a space's permission comes from.
    PermissionSource {
        /// Its agent's mode, with `[permissions]` and the key's default.
        Mode => "mode",
        /// The space above it, whose permission is narrower.
        Parent => "parent",
    }
);

/// One effective permission and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permission<T> {
    pub value: T,
    pub from: PermissionSource,
}

impl<T: Ord + Copy> Permission<T> {
    fn of_mode(value: Option<T>) -> Self {
        Self {
            value: value.expect("every key has a default"),
            from: PermissionSource::Mode,
        }
    }

    /// This permission of a space below one with `parent`: the parent's
    /// where it is narrower.
    fn below(self, parent: Self) -> Self {
        match parent.value < self.value {
            true => Self {
                value: parent.value,
                from: PermissionSource::Parent,
            },
            false => self,
        }
    }
}

/// A space's effective permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub issue_open: Permission<Opening>,
    pub pr_open: Permission<Opening>,
    pub issue_other_repositories: Permission<OtherRepositories>,
    pub merge: Permission<Opening>,
    pub push: Permission<Pushes>,
    pub api_writes: Permission<ApiWrites>,
}

impl Permissions {
    /// These permissions of a space below one with `parent`, none wider
    /// than the parent's.
    pub fn below(self, parent: &Self) -> Self {
        Self {
            issue_open: self.issue_open.below(parent.issue_open),
            pr_open: self.pr_open.below(parent.pr_open),
            issue_other_repositories: (self.issue_other_repositories)
                .below(parent.issue_other_repositories),
            merge: self.merge.below(parent.merge),
            push: self.push.below(parent.push),
            api_writes: self.api_writes.below(parent.api_writes),
        }
    }

    /// Each key with its value and where it comes from, in config order.
    pub fn entries(&self) -> [(&'static str, &'static str, PermissionSource); 6] {
        fn entry<T: Copy>(
            key: &'static str,
            permission: Permission<T>,
            name: fn(T) -> &'static str,
        ) -> (&'static str, &'static str, PermissionSource) {
            (key, name(permission.value), permission.from)
        }
        [
            entry("issue_open", self.issue_open, Opening::as_str),
            entry("pr_open", self.pr_open, Opening::as_str),
            entry(
                "issue_other_repositories",
                self.issue_other_repositories,
                OtherRepositories::as_str,
            ),
            entry("merge", self.merge, Opening::as_str),
            entry("push", self.push, Pushes::as_str),
            entry("api_writes", self.api_writes, ApiWrites::as_str),
        ]
    }

    fn of_mode(layered: PermissionsConfig) -> Self {
        Self {
            issue_open: Permission::of_mode(layered.issue_open),
            pr_open: Permission::of_mode(layered.pr_open),
            issue_other_repositories: Permission::of_mode(layered.issue_other_repositories),
            merge: Permission::of_mode(layered.merge),
            push: Permission::of_mode(layered.push),
            api_writes: Permission::of_mode(layered.api_writes),
        }
    }
}

/// The mode an agent of a space started in, which resolves as a space of
/// `kind` with or without a repository.
#[derive(Debug, Clone, Copy)]
pub struct SpaceMode<'a> {
    pub name: &'a str,
    pub kind: SpaceKind,
    pub repository: bool,
}

/// The permissions of a space in `mode` under `settings`, before the tree
/// narrows them; a space without a recorded mode has `[permissions]` and
/// the keys' defaults. Each key resolves, highest first, from
/// `[modes.NAME.permissions]`, `[permissions]`, the built-in mode's
/// defaults, then the key's default.
pub fn resolve(settings: &Settings, mode: Option<SpaceMode>) -> Result<Permissions> {
    let (configured, built_in) = match mode {
        Some(mode) => modes::permission_layers(&settings.modes, mode)?,
        None => Default::default(),
    };
    let mut layered = PermissionsConfig::KEY_DEFAULTS;
    for layer in [built_in, settings.permissions, configured] {
        layered.merge(layer, Merge::SameLevel);
    }
    Ok(Permissions::of_mode(layered))
}

#[cfg(test)]
mod tests;
