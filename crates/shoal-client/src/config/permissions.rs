//! `[permissions]` and `[modes.NAME.permissions]`: what a scoped agent may do
//! at once, after approval, or not at all. Each value is ordered from the
//! narrowest to the widest, `deny` < `propose` < `direct`. A space has its
//! mode's, each no wider than its parent's, then each key a `--permission`
//! given to its agent sets, as given.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{
    config::options::{Merge, Options},
    model::SpaceKind,
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

states!(
    #[derive(PartialOrd, Ord)]
    /// Whether a scoped caller takes an action at once, after approval, or
    /// not at all.
    Acting: Variants {
        Deny => "deny",
        Propose => "propose",
        Direct => "direct",
    }
);

states!(
    #[derive(PartialOrd, Ord)]
    /// Whether a scoped caller may take an action, which nothing asks
    /// approval for.
    Allowed {
        Deny => "deny",
        Direct => "direct",
    }
);

/// Every permission key, once: its field, its [`PermissionKey`] variant, the
/// name config gives it and its type of value. It defines
/// [`PermissionsConfig`], [`PermissionKey`] and [`Permissions`] and what
/// reads or narrows them key by key.
macro_rules! permission_keys {
    ($($(#[$doc:meta])* $field:ident: $key:ident => $name:literal, $value:ty;)+) => {
        /// What a scoped agent may do at once, after approval, or not at all, as
        /// one layer writes it; resolving the config fills what no layer sets.
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Options)]
        #[serde(default, deny_unknown_fields)]
        #[options(resolved = Self)]
        pub struct PermissionsConfig {
            $(
                $(#[$doc])*
                #[serde(skip_serializing_if = "Option::is_none")]
                pub $field: Option<$value>,
            )+
        }

        states!(
            /// A permission, by the key config names it with.
            PermissionKey: Variants {
                $($key => $name,)+
            }
        );

        /// A space's effective permissions.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Permissions {
            $(pub $field: Permission<$value>,)+
        }

        impl PermissionsConfig {
            /// The value this narrowing sets for `key`, as config names it.
            pub fn get(&self, key: PermissionKey) -> Option<&'static str> {
                match key {
                    $(PermissionKey::$key => self.$field.map(<$value>::as_str),)+
                }
            }

            /// Refuse a key these permissions set wider than the giver's own
            /// permission for it, `held`, since a scoped caller gives only what it
            /// holds itself.
            pub fn ensure_held(&self, held: &Permissions) -> Result<()> {
                $(within(PermissionKey::$key, self.$field, held.$field)?;)+
                Ok(())
            }

            /// These permissions without the value they set for `key`.
            fn without(mut self, key: PermissionKey) -> Self {
                match key {
                    $(PermissionKey::$key => self.$field = None,)+
                }
                self
            }
        }

        impl Permissions {
            /// These permissions with each key `given` sets, as given.
            pub fn given(self, given: &PermissionsConfig) -> Self {
                Self {
                    $($field: self.$field.given(given.$field),)+
                }
            }

            /// These permissions of a space below one with `parent`, none wider
            /// than the parent's.
            pub fn below(self, parent: &Self) -> Self {
                Self {
                    $($field: self.$field.below(parent.$field),)+
                }
            }

            /// The value of each key.
            pub fn values(&self) -> PermissionsConfig {
                PermissionsConfig {
                    $($field: Some(self.$field.value),)+
                }
            }

            /// The value of `key`, as config names it, and where it comes from.
            pub fn get(&self, key: PermissionKey) -> (&'static str, PermissionSource) {
                match key {
                    $(PermissionKey::$key => (self.$field.value.as_str(), self.$field.from),)+
                }
            }

            /// The permissions `layered` gives, as a mode's.
            pub fn of_mode(layered: PermissionsConfig) -> Self {
                Self {
                    $($field: Permission::of_mode(layered.$field),)+
                }
            }
        }
    };
}

permission_keys! {
    issue_open: IssueOpen => "issue_open", Opening;
    pr_open: PrOpen => "pr_open", Opening;
    /// Opening an issue in another repository than the space's.
    issue_other_repositories: IssueOtherRepositories => "issue_other_repositories", OtherRepositories;
    /// Merging a branch with `shoal merge`.
    merge: Merge => "merge", Opening;
    /// Pushing to the workspace repository's remotes, with `shoal pr open`
    /// or plain `git push`.
    push: Push => "push", Pushes;
    /// Forge API requests through `shoal forge api` that are not reads.
    api_writes: ApiWrites => "api_writes", ApiWrites;
    /// Installing, starting, stopping and restarting the daemon, and
    /// updating Shoal.
    daemon: Daemon => "daemon", Acting;
    /// Administering Shoal: its config, repositories, cleanup, the repair,
    /// adoption, restoring and moving of workspaces.
    admin: Admin => "admin", Acting;
    /// Starting agents regardless of `max_agents` and memory.
    limits: Limits => "limits", Acting;
    /// Removing or converting a space the user holds, and releasing the
    /// user's holds.
    user_holds: UserHolds => "user_holds", Acting;
    /// Reading, clearing and watching the user's notifications, the
    /// daemon-wide problems the inbox lists, and Shoal's own log.
    notifications: Notifications => "notifications", Allowed;
}

/// Refuse `value` of `key` where it is wider than `held`.
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

impl PermissionsConfig {
    /// Each key's value where no mode or config sets one.
    pub const KEY_DEFAULTS: Self = Self {
        issue_open: Some(Opening::Direct),
        pr_open: Some(Opening::Direct),
        issue_other_repositories: Some(OtherRepositories::Propose),
        merge: Some(Opening::Propose),
        push: Some(Pushes::Direct),
        api_writes: Some(ApiWrites::Deny),
        daemon: Some(Acting::Deny),
        admin: Some(Acting::Propose),
        limits: Some(Acting::Propose),
        user_holds: Some(Acting::Propose),
        notifications: Some(Allowed::Deny),
    };

    /// The defaults of a mode that coordinates rather than edits: it decides
    /// its children's merges and issues elsewhere.
    pub const COORDINATING: Self = Self {
        issue_other_repositories: Some(OtherRepositories::Direct),
        merge: Some(Opening::Direct),
        ..Self::KEY_DEFAULTS
    };

    /// The defaults of a master session, which reaches every space and does
    /// what the user would, but proposes what stops every agent.
    pub const ORCHESTRATING: Self = Self {
        daemon: Some(Acting::Propose),
        admin: Some(Acting::Direct),
        limits: Some(Acting::Direct),
        user_holds: Some(Acting::Direct),
        notifications: Some(Allowed::Direct),
        ..Self::COORDINATING
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

    /// The `--permission KEY=VALUE` assignments that give this narrowing.
    pub fn assignments(&self) -> Vec<String> {
        (PermissionKey::ALL.iter())
            .filter_map(|&key| Some(format!("{key}={}", self.get(key)?)))
            .collect()
    }

    /// One change per key this narrowing sets, each setting that key alone,
    /// as one `--permission KEY=VALUE` gives it.
    pub fn changes(&self) -> Vec<PermissionChange> {
        (PermissionKey::ALL.iter())
            .filter(|&&key| self.get(key).is_some())
            .map(|&key| PermissionChange {
                set: self.only(&[key]),
                dropped: Vec::new(),
            })
            .collect()
    }

    /// These permissions with only the values they set for `keys`.
    fn only(self, keys: &[PermissionKey]) -> Self {
        (PermissionKey::ALL.iter())
            .filter(|key| !keys.contains(key))
            .fold(self, |only, &key| only.without(key))
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

    /// The `--permission KEY=VALUE` assignments that make this change,
    /// `KEY=mode` for each key it drops.
    pub fn assignments(&self) -> Vec<String> {
        let dropped = (self.dropped.iter()).map(|key| format!("{key}={MODE_VALUE}"));
        self.set.assignments().into_iter().chain(dropped).collect()
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

impl Permissions {
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
        (keys.iter())
            .find(|&&key| self.acting(key) != Acting::Direct)
            .map(|&key| self.cited(key))
    }

    /// What `key` lets the space do, whichever of its values the key takes.
    pub fn acting(&self, key: PermissionKey) -> Acting {
        let (value, _) = self.get(key);
        (Acting::ALL.iter().copied())
            .find(|acting| acting.as_str() == value)
            .expect("every value is deny, propose or direct")
    }

    /// The permission `key` as a refusal cites it, with where it comes from.
    pub fn cited(&self, key: PermissionKey) -> String {
        let (value, from) = self.get(key);
        cite(key.as_str(), &value, from)
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
