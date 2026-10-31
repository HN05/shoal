//! The config files parsed strictly: an unknown key, or a key in the wrong
//! file, fails with its line and column. The global file is `[machine]` plus
//! the repository schema; each part is deserialized on its own, since
//! `serde(flatten)` would accept unknown keys and lose error positions.
use serde::de::{DeserializeSeed, Error as _, IgnoredAny, MapAccess, Visitor};
use toml::{
    Spanned,
    de::{DeTable, DeValue, Deserializer, Error, ValueDeserializer},
};

use super::{machine::Machine, options::Options, repo::RepoConfig};

const MACHINE: &str = "machine";

/// Settings Shoal no longer reads, each refused with what replaces it.
const REMOVED: &[(&[&str], &str)] = &[
    (
        &["machine", "bitbucket"],
        "[machine.bitbucket] moved: set token and email under [machine.forges.\"bitbucket.org\"]",
    ),
    (
        &["agent", "forge_clis", "fj_home"],
        "agent.forge_clis.fj_home is gone: set agent_token under [machine.forges.\"<host>\"] in the global config",
    ),
];

/// Fail at the first setting of `root` that [`REMOVED`] names.
fn refuse_removed(root: &Spanned<DeTable<'_>>) -> Result<(), Error> {
    for (path, message) in REMOVED {
        refuse(root, path, message)?;
    }
    Ok(())
}

pub fn global(text: &str) -> Result<(Machine, RepoConfig), Error> {
    let positioned = |mut error: Error| {
        error.set_input(Some(text));
        error
    };
    let mut root = DeTable::parse(text)?;
    refuse_removed(&root).map_err(positioned)?;
    let mut repository_only = Vec::new();
    RepoConfig::repository_only("", &mut repository_only);
    for key in &repository_only {
        let path: Vec<_> = key.split('.').collect();
        refuse(
            &root,
            &path,
            &format!("{key} belongs in the repository config"),
        )
        .map_err(positioned)?;
    }
    let machine = match root.get_mut().remove(MACHINE) {
        Some(value) => {
            serde::Deserialize::deserialize(ValueDeserializer::from(value)).map_err(positioned)?
        }
        None => Machine::default(),
    };
    let repository =
        serde::Deserialize::deserialize(Deserializer::from(root)).map_err(positioned)?;
    Ok((machine, repository))
}

pub fn repository(text: &str) -> Result<RepoConfig, Error> {
    let root = DeTable::parse(text)?;
    refuse(&root, &[MACHINE], "[machine] belongs in the global config")
        .and_then(|()| refuse_removed(&root))
        .map_err(|mut error| {
            error.set_input(Some(text));
            error
        })?;
    serde::Deserialize::deserialize(Deserializer::from(root)).map_err(|mut error: Error| {
        error.set_input(Some(text));
        error
    })
}

/// Fail with `message` at the value `path` names, when `root` sets it.
fn refuse(root: &Spanned<DeTable<'_>>, path: &[&str], message: &str) -> Result<(), Error> {
    if !contains(root.get_ref(), path) {
        return Ok(());
    }
    Refusal { path, message }.deserialize(Deserializer::from(root.clone()))
}

fn contains(table: &DeTable<'_>, path: &[&str]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return true;
    };
    match table.get(*first).map(|value| value.get_ref()) {
        Some(_) if rest.is_empty() => true,
        Some(DeValue::Table(table)) => contains(table, rest),
        _ => false,
    }
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
