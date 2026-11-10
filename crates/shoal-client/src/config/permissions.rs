//! `[permissions]` and `[modes.NAME.permissions]`: what a scoped agent may do
//! at once, after approval, or not at all. Each value is ordered from the
//! narrowest to the widest, `deny` < `propose` < `direct`. A space has its
//! mode's, each no wider than its parent's, then each key a `--permission`
//! given to its agent sets, as given.
use anyhow::{Context, Result, bail};
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Self)]
pub struct PermissionsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_open: Option<Opening>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_open: Option<Opening>,
    /// Opening an issue in another repository than the space's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_other_repositories: Option<OtherRepositories>,
    /// Merging a branch with `shoal merge`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge: Option<Opening>,
    /// Pushing to the workspace repository's remotes, with `shoal pr open`
    /// or plain `git push`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push: Option<Pushes>,
    /// Forge API requests through `shoal forge api` that are not reads.
    #[serde(skip_serializing_if = "Option::is_none")]
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

    /// The narrowing `--permission KEY=VALUE` gives with each of
    /// `assignments`, a later one of a key replacing an earlier one,
    /// refusing an unknown key or value.
    pub fn assigned(assignments: &[String]) -> Result<Self> {
        let mut narrowing = Self::default();
        for assignment in assignments {
            let (key, value) = (assignment.split_once('='))
                .with_context(|| format!("--permission {assignment}: give KEY=VALUE"))?;
            let one: Self = toml::from_str(&format!("{} = {:?}", key.trim(), value.trim()))
                .with_context(|| format!("--permission {assignment}"))?;
            narrowing.merge(one, Merge::SameLevel);
        }
        Ok(narrowing)
    }

    /// Whether the narrowing sets no key.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The value this narrowing sets for `key`, as config names it.
    pub fn get(&self, key: PermissionKey) -> Option<&'static str> {
        match key {
            PermissionKey::IssueOpen => self.issue_open.map(Opening::as_str),
            PermissionKey::PrOpen => self.pr_open.map(Opening::as_str),
            PermissionKey::IssueOtherRepositories => {
                (self.issue_other_repositories).map(OtherRepositories::as_str)
            }
            PermissionKey::Merge => self.merge.map(Opening::as_str),
            PermissionKey::Push => self.push.map(Pushes::as_str),
            PermissionKey::ApiWrites => self.api_writes.map(ApiWrites::as_str),
        }
    }

    /// The `--permission KEY=VALUE` assignments that give this narrowing.
    pub fn assignments(&self) -> Vec<String> {
        (PermissionKey::ALL.iter())
            .filter_map(|&key| Some(format!("{key}={}", self.get(key)?)))
            .collect()
    }

    /// Refuse a key these permissions set wider than the giver's own
    /// permission for it, `held`, since a scoped caller gives only what it
    /// holds itself.
    pub fn ensure_held(&self, held: &Permissions) -> Result<()> {
        fn within<T: Ord + Copy + std::fmt::Display>(
            key: PermissionKey,
            value: Option<T>,
            held: Permission<T>,
        ) -> Result<()> {
            match value {
                Some(value) if value > held.value => bail!(
                    "--permission {key}={value} gives more than the giver holds: {}; a space gives only what it holds",
                    held.cited(key.as_str())
                ),
                _ => Ok(()),
            }
        }
        within(PermissionKey::IssueOpen, self.issue_open, held.issue_open)?;
        within(PermissionKey::PrOpen, self.pr_open, held.pr_open)?;
        within(
            PermissionKey::IssueOtherRepositories,
            self.issue_other_repositories,
            held.issue_other_repositories,
        )?;
        within(PermissionKey::Merge, self.merge, held.merge)?;
        within(PermissionKey::Push, self.push, held.push)?;
        within(PermissionKey::ApiWrites, self.api_writes, held.api_writes)
    }

    /// These permissions with only the values they set for `keys`.
    fn only(self, keys: &[PermissionKey]) -> Self {
        (PermissionKey::ALL.iter())
            .filter(|key| !keys.contains(key))
            .fold(self, |only, &key| only.without(key))
    }

    /// These permissions without the value they set for `key`.
    fn without(mut self, key: PermissionKey) -> Self {
        match key {
            PermissionKey::IssueOpen => self.issue_open = None,
            PermissionKey::PrOpen => self.pr_open = None,
            PermissionKey::IssueOtherRepositories => self.issue_other_repositories = None,
            PermissionKey::Merge => self.merge = None,
            PermissionKey::Push => self.push = None,
            PermissionKey::ApiWrites => self.api_writes = None,
        }
        self
    }
}

/// What `--permission` assignments change in the permissions an agent was
/// given: each key `set` names takes its value, each `KEY=mode` in `dropped`
/// loses the one it was given, and the rest keep theirs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionChange {
    #[serde(default, skip_serializing_if = "PermissionsConfig::is_empty")]
    pub set: PermissionsConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<PermissionKey>,
}

impl PermissionChange {
    /// The change `--permission KEY=VALUE` makes, where `mode` as the value
    /// drops the key's given value; refusing an unknown key or value.
    pub fn assigned(assignment: &str) -> Result<Self> {
        match assignment.split_once('=') {
            Some((key, value)) if value.trim() == MODE_VALUE => {
                let key = (PermissionKey::ALL.iter())
                    .find(|known| known.as_str() == key.trim())
                    .with_context(|| format!("--permission {assignment}: unknown key"))?;
                Ok(Self {
                    set: PermissionsConfig::default(),
                    dropped: vec![*key],
                })
            }
            _ => Ok(Self {
                set: PermissionsConfig::assigned(&[assignment.to_owned()])?,
                dropped: Vec::new(),
            }),
        }
    }

    /// The change that turns the permissions `recorded` gives into those
    /// `given` gives, which a registration or restore brings in rather than
    /// keeps: each key `given` sets to another value, and each `recorded`
    /// sets that `given` drops.
    pub fn between(recorded: &PermissionsConfig, given: &PermissionsConfig) -> Self {
        let keys = PermissionKey::ALL.iter().copied();
        let changed = keys.filter(|&key| given.get(key) != recorded.get(key));
        let (set, dropped): (Vec<_>, Vec<_>) = changed.partition(|&key| given.get(key).is_some());
        Self {
            set: given.only(&set),
            dropped,
        }
    }

    /// One change of `changes`, a later one of a key replacing an earlier
    /// one.
    pub fn combined(changes: &[Self]) -> Self {
        let mut combined = Self::default();
        for change in changes {
            for &key in &change.dropped {
                combined.set = combined.set.without(key);
                if !combined.dropped.contains(&key) {
                    combined.dropped.push(key);
                }
            }
            combined
                .dropped
                .retain(|&key| change.set.get(key).is_none());
            combined.set.merge(change.set, Merge::SameLevel);
        }
        combined
    }

    /// Refuse a key this change sets wider than `held`, a scoped giver's
    /// own permissions, or drops to a wider value of `fallback`, those the
    /// agent has without what it was given.
    pub fn ensure_held(&self, fallback: &Permissions, held: &Permissions) -> Result<()> {
        self.set.ensure_held(held)?;
        for &key in &self.dropped {
            if fallback.values().only(&[key]).ensure_held(held).is_err() {
                let ((value, _), (holds, from)) = (fallback.get(key), held.get(key));
                bail!(
                    "--permission {key}={MODE_VALUE} would give {key}={value}, which the giver does not hold: {}; a space gives only what it holds",
                    cite(key.as_str(), &holds, from)
                );
            }
        }
        Ok(())
    }

    /// The permissions `given`, an agent was given, after this change.
    pub fn applied_to(&self, given: PermissionsConfig) -> PermissionsConfig {
        let mut applied = (self.dropped.iter()).fold(given, |given, &key| given.without(key));
        applied.merge(self.set, Merge::SameLevel);
        applied
    }
}

/// The value `--permission KEY=mode` gives to drop a key's given value.
pub const MODE_VALUE: &str = "mode";

states!(
    /// A permission, by the key config names it with.
    PermissionKey: Variants {
        IssueOpen => "issue_open",
        PrOpen => "pr_open",
        IssueOtherRepositories => "issue_other_repositories",
        Merge => "merge",
        Push => "push",
        ApiWrites => "api_writes",
    }
);

states!(
    /// Where a space's permission comes from.
    PermissionSource {
        /// Its agent's mode, with `[permissions]` and the key's default.
        Mode => "mode",
        /// The `--permission` given to its agent when it started, resumed or
        /// restarted.
        Given => "given",
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

    /// This permission set to `value`, where a `--permission` given to the
    /// agent sets one, whatever its mode and the space above allow.
    fn given(self, value: Option<T>) -> Self {
        match value {
            Some(value) => Self {
                value,
                from: PermissionSource::Given,
            },
            None => self,
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

impl<T: std::fmt::Display> Permission<T> {
    /// The permission `key` as a refusal cites it, with where it comes from.
    pub fn cited(&self, key: &str) -> String {
        cite(key, &self.value, self.from)
    }
}

/// The permission `key` with `value` from `from`, as a refusal cites it.
fn cite(key: &str, value: &dyn std::fmt::Display, from: PermissionSource) -> String {
    let from = match from {
        PermissionSource::Mode => "its agent's mode",
        PermissionSource::Given => "the --permission given to its agent",
        PermissionSource::Parent => "the space above it",
    };
    format!("the space's {key} permission is \"{value}\", from {from}")
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
    /// These permissions with each key `given` sets, as given.
    pub fn given(self, given: &PermissionsConfig) -> Self {
        Self {
            issue_open: self.issue_open.given(given.issue_open),
            pr_open: self.pr_open.given(given.pr_open),
            issue_other_repositories: (self.issue_other_repositories)
                .given(given.issue_other_repositories),
            merge: self.merge.given(given.merge),
            push: self.push.given(given.push),
            api_writes: self.api_writes.given(given.api_writes),
        }
    }

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

    /// The value of each key.
    pub fn values(&self) -> PermissionsConfig {
        PermissionsConfig {
            issue_open: Some(self.issue_open.value),
            pr_open: Some(self.pr_open.value),
            issue_other_repositories: Some(self.issue_other_repositories.value),
            merge: Some(self.merge.value),
            push: Some(self.push.value),
            api_writes: Some(self.api_writes.value),
        }
    }

    /// The value of `key`, as config names it, and where it comes from.
    pub fn get(&self, key: PermissionKey) -> (&'static str, PermissionSource) {
        fn of<T: Copy>(
            permission: Permission<T>,
            name: fn(T) -> &'static str,
        ) -> (&'static str, PermissionSource) {
            (name(permission.value), permission.from)
        }
        match key {
            PermissionKey::IssueOpen => of(self.issue_open, Opening::as_str),
            PermissionKey::PrOpen => of(self.pr_open, Opening::as_str),
            PermissionKey::IssueOtherRepositories => {
                of(self.issue_other_repositories, OtherRepositories::as_str)
            }
            PermissionKey::Merge => of(self.merge, Opening::as_str),
            PermissionKey::Push => of(self.push, Pushes::as_str),
            PermissionKey::ApiWrites => of(self.api_writes, ApiWrites::as_str),
        }
    }

    /// Each key with its value and where it comes from, in config order.
    pub fn entries(&self) -> impl Iterator<Item = (PermissionKey, &'static str, PermissionSource)> {
        (PermissionKey::ALL.iter()).map(|&key| {
            let (value, from) = self.get(key);
            (key, value, from)
        })
    }

    /// The first of `keys` these permissions hold below `direct`, cited;
    /// none when every one is `direct`.
    pub fn withheld(&self, keys: &[PermissionKey]) -> Option<String> {
        keys.iter().find_map(|&key| {
            let (value, from) = self.get(key);
            (value != Opening::Direct.as_str()).then(|| cite(key.as_str(), &value, from))
        })
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
    pub name: &'a crate::config::modes::ModeName,
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
