//! The config files parsed strictly: an unknown key, or a key in the wrong
//! file, fails with its line and column. The global file is `[machine]` plus
//! the repository schema; each part is deserialized on its own, since
//! `serde(flatten)` would accept unknown keys and lose error positions.
use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, Visitor};
use toml::{
    Spanned,
    de::{DeTable, DeValue, Deserializer, Error, ValueDeserializer},
};

use super::{
    machine::{self, Machine},
    repo::RepoConfig,
    same_as::References,
};

const MACHINE: &str = "machine";

/// The key a file's root may not set, since its layer keeps the file's
/// references there.
const SAME_AS: &str = "same_as";

/// Settings Shoal no longer reads, each refused with what replaces it; `*`
/// stands for any key.
const REMOVED: &[(&[&str], &str)] = &[
    (
        &["machine", "bitbucket"],
        "[machine.bitbucket] moved: set user_token and user_email under [machine.forges.\"bitbucket.org\"]",
    ),
    (
        &["machine", "forges", "*", "token"],
        "token is now user_token under [machine.forges.\"<host>\"]",
    ),
    (
        &["machine", "forges", "*", "email"],
        "email is now user_email under [machine.forges.\"<host>\"]",
    ),
    (&["idle_stop"], "[idle_stop] enabled is now [sleep] enabled"),
    (
        &["sleep", "idle"],
        "[sleep] idle is gone: idle agents sleep unless you are engaged with them, as [sleep] enabled sets",
    ),
    (
        &["agent", "forge_clis"],
        "[agent.forge_clis] is gone: Shoal's commands act as the agent account, so set agent_token under [machine.forges.\"<host>\"] in the global config",
    ),
    (
        &["agent", "templates"],
        "[agent.templates] is gone: add text to [rules.worker], [rules.chat] or [rules.review], or to a mode's instructions under [modes.<name>]",
    ),
    // The packaged template set both of these, so most global configs do.
    (
        &["agents", "claude", "default_mode"],
        "agents.claude.default_mode is gone: agents always start as CLI agents; open a desktop app with shoal open, and set its default with app.default",
    ),
    (
        &["agents", "codex", "default_mode"],
        "agents.codex.default_mode is gone: agents always start as CLI agents; open a desktop app with shoal open, and set its default with app.default",
    ),
    (
        &["terminal", "happy"],
        "[terminal.happy] moved: set enabled under [agents.happy]",
    ),
    (
        &["machine", "max_agents"],
        "machine.max_agents is now machine.max_heavy_agents",
    ),
    (
        &["issue", "plan_label"],
        "[issue] plan_label is gone: set label under [modes.NAME], such as [modes.plan] label = \"needs-plan\"",
    ),
    (
        &["issue", "planned_label"],
        "[issue] planned_label is gone: set label under [modes.NAME], such as [modes.worker] label = \"planned\"",
    ),
    (
        &["agent", "max_running"],
        "agent.max_running is now agent.max_heavy_running",
    ),
];

/// Fail at the first setting of `root` that [`REMOVED`] names.
fn refuse_removed(root: &Spanned<DeTable<'_>>) -> Result<(), Error> {
    for (path, message) in REMOVED {
        refuse(root, path, message)?;
    }
    Ok(())
}

/// One global file as written: its `[machine]` table and its values for
/// the options a repository may override.
#[derive(Debug, Default)]
pub(crate) struct GlobalFile {
    pub machine: machine::Written,
    pub repository: RepoConfig,
}

pub(crate) fn global(text: &str) -> Result<GlobalFile, Error> {
    global_file(text, |_| Ok(()))
}

/// The shared config, which takes every key of the global file but the
/// settings a machine reaches the hub with, which it needs before it has
/// the shared file.
pub(crate) fn shared(text: &str) -> Result<GlobalFile, Error> {
    global_file(text, |root| {
        for key in HUB_REACH {
            let message = format!(
                "machine.hub.{key} belongs in each machine's own config.toml, since a machine reaches the hub before it has the shared config"
            );
            refuse(root, &[MACHINE, "hub", key], &message)?;
        }
        Ok(())
    })
}

/// The settings of `[machine.hub]` that say how a machine reaches the hub.
const HUB_REACH: [&str; 4] = ["enabled", "listen", "url", "connect"];

/// A global file, after `refusals` checked what it may not set.
fn global_file(
    text: &str,
    refusals: impl Fn(&Spanned<DeTable<'_>>) -> Result<(), Error>,
) -> Result<GlobalFile, Error> {
    let positioned = |mut error: Error| {
        error.set_input(Some(text));
        error
    };
    let mut root = DeTable::parse(text)?;
    refusals(&root)
        .and_then(|()| refuse_removed(&root))
        .and_then(|()| refuse_root_same_as(&root))
        .map_err(positioned)?;
    let machine = match root.get_mut().remove(MACHINE) {
        Some(mut value) => {
            let same_as = match value.get_mut() {
                DeValue::Table(table) => References::take(table),
                _ => References::default(),
            };
            // Read alone for its errors' positions, which the tables the
            // files merge into no longer have.
            let _: Machine =
                serde::Deserialize::deserialize(ValueDeserializer::from(value.clone()))
                    .map_err(positioned)?;
            let table = serde::Deserialize::deserialize(ValueDeserializer::from(value))
                .map_err(positioned)?;
            machine::Written::new(table, same_as)
        }
        None => machine::Written::default(),
    };
    let same_as = References::take(root.get_mut());
    let repository: RepoConfig =
        serde::Deserialize::deserialize(Deserializer::from(root)).map_err(positioned)?;
    Ok(GlobalFile {
        machine,
        repository: RepoConfig {
            same_as,
            ..repository
        },
    })
}

pub fn repository(text: &str) -> Result<RepoConfig, Error> {
    let positioned = |mut error: Error| {
        error.set_input(Some(text));
        error
    };
    let mut root = DeTable::parse(text)?;
    refuse(&root, &[MACHINE], "[machine] belongs in the global config")
        .and_then(|()| refuse_mode_repositories(&root))
        .and_then(|()| refuse_trigger_repositories(&root))
        .and_then(|()| refuse_removed(&root))
        .and_then(|()| refuse_root_same_as(&root))
        .map_err(positioned)?;
    let same_as = References::take(root.get_mut());
    let repository: RepoConfig =
        serde::Deserialize::deserialize(Deserializer::from(root)).map_err(positioned)?;
    Ok(RepoConfig {
        same_as,
        ..repository
    })
}

/// Refuse `same_as` as a key of the root, which names no option.
fn refuse_root_same_as(root: &Spanned<DeTable<'_>>) -> Result<(), Error> {
    refuse(root, &[SAME_AS], "same_as is not an option")
}

/// Refuse a mode's `repository` in a repository config: a mode that starts a
/// chat without a repository is the global config's, since no repository's
/// config applies to that chat.
fn refuse_mode_repositories(root: &Spanned<DeTable<'_>>) -> Result<(), Error> {
    let Some(DeValue::Table(modes)) = root.get_ref().get("modes").map(Spanned::get_ref) else {
        return Ok(());
    };
    for name in modes.keys() {
        let name = name.get_ref();
        let message = format!("modes.{name}.repository belongs in the global config");
        refuse(root, &["modes", name, "repository"], &message)?;
    }
    Ok(())
}

/// Refuse a trigger's `repository` in a repository config, whose triggers
/// start in that repository.
fn refuse_trigger_repositories(root: &Spanned<DeTable<'_>>) -> Result<(), Error> {
    let message = "a repository's trigger starts in that repository; repository belongs to a trigger in the global config";
    refuse(root, &["triggers", "*", "repository"], message)
}

/// Fail with `message` at the first value `path` names, when `root` sets
/// one.
fn refuse(root: &Spanned<DeTable<'_>>, path: &[&str], message: &str) -> Result<(), Error> {
    let Some(found) = find(root.get_ref(), path) else {
        return Ok(());
    };
    let path: Vec<_> = found.iter().map(String::as_str).collect();
    Refusal {
        path: &path,
        message,
    }
    .deserialize(Deserializer::from(root.clone()))
}

/// The keys of the first value of `table` that `path` names, where `*`
/// stands for any key.
fn find(table: &DeTable<'_>, path: &[&str]) -> Option<Vec<String>> {
    let Some((first, rest)) = path.split_first() else {
        return Some(Vec::new());
    };
    table
        .iter()
        .filter(|(key, _)| *first == "*" || key.get_ref() == first)
        .find_map(|(key, value)| {
            let mut found = match value.get_ref() {
                _ if rest.is_empty() => Vec::new(),
                DeValue::Table(table) => find(table, rest)?,
                _ => return None,
            };
            found.insert(0, key.get_ref().to_string());
            Some(found)
        })
}

/// Walks to the value at `path` and fails there, so the error carries that
/// value's position.
struct Refusal<'a> {
    path: &'a [&'a str],
    message: &'a str,
}

impl<'de> DeserializeSeed<'de> for Refusal<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        if self.path.is_empty() {
            return Err(D::Error::custom(self.message));
        }
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Refusal<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a table")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let (first, rest) = self.path.split_first().expect("a nonempty path");
        while let Some(key) = map.next_key::<String>()? {
            if key == *first {
                map.next_value_seed(Refusal {
                    path: rest,
                    message: self.message,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
