//! Layering for config structs that `#[derive(Options)]`: one declaration of
//! each option drives its built-in default, its merge across layers and its
//! provenance, so a reported layer is the one whose value is in effect.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use super::repo::ConfigLayer as Layer;

pub use shoal_config_derive::Options;

/// A config struct as one layer writes it, and its resolved form.
pub trait Options: Default + Clone {
    type Resolved;

    /// The lowest layer: the built-in defaults.
    fn built_in() -> Self;

    /// Layer `top` over `self`: an option `top` sets wins, an omitted one
    /// falls through, and a named entry replaces the one below it whole.
    fn merge(&mut self, top: Self, merge: Merge);

    /// The merged layers as settings; fails for an option without a value
    /// that needs one. `prefix` is this struct's key.
    fn resolve(self, prefix: &str) -> Result<Self::Resolved>;

    /// Every option with its value and the layer that supplied it, from
    /// `sources` highest first.
    fn entries(prefix: &str, sources: &[(&Self, Layer)], entries: &mut Vec<Entry>) -> Result<()>;

    /// The keys only a repository config may set.
    fn repository_only(prefix: &str, keys: &mut Vec<String>);
}

/// Which layers a merge combines; only templates tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// Layers of one level: the repository's, or the global and built-in.
    SameLevel,
    /// The repository's layers over the global ones, which a repository
    /// template adds to.
    RepositoryOverGlobal,
}

/// One effective value and the layer that supplied it.
#[derive(Debug, Serialize)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub layer: Layer,
    /// The Markdown file the layer read a template from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

pub fn key(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

pub fn required<T>(value: Option<T>, key: &str) -> Result<T> {
    value.with_context(|| format!("{key} has no built-in default"))
}

pub fn scalar<T>(base: &mut Option<T>, top: Option<T>) {
    if top.is_some() {
        *base = top;
    }
}

pub fn list<T>(base: &mut Vec<T>, top: Vec<T>) {
    if !top.is_empty() {
        *base = top;
    }
}

pub fn template(base: &mut Option<String>, top: Option<String>, merge: Merge) {
    match merge {
        Merge::SameLevel => scalar(base, top),
        Merge::RepositoryOverGlobal => *base = append_template(base.take(), top),
    }
}

fn append_template(base: Option<String>, addition: Option<String>) -> Option<String> {
    match (base, addition) {
        (Some(base), Some(addition)) if !base.is_empty() && !addition.is_empty() => {
            Some(format!("{base}\n\n{addition}"))
        }
        (Some(base), Some(addition)) if base.is_empty() => Some(addition),
        (Some(base), _) => Some(base),
        (None, addition) => addition,
    }
}

/// The resolved form of a `named_sections` map.
pub trait SectionMap {
    type Resolved;
}

impl<T: Options> SectionMap for BTreeMap<String, T> {
    type Resolved = BTreeMap<String, T::Resolved>;
}

pub fn named_sections<T: Options>(
    base: &mut BTreeMap<String, T>,
    top: BTreeMap<String, T>,
    merge: Merge,
) {
    for (name, entry) in top {
        base.entry(name).or_default().merge(entry, merge);
    }
}

/// Each entry over the section's built-in defaults, so an entry no layer
/// below names still resolves.
pub fn resolve_sections<T: Options>(
    sections: BTreeMap<String, T>,
    prefix: &str,
) -> Result<BTreeMap<String, T::Resolved>> {
    sections
        .into_iter()
        .map(|(name, entry)| {
            let mut section = T::built_in();
            section.merge(entry, Merge::SameLevel);
            let resolved = section.resolve(&key(prefix, &name))?;
            Ok((name, resolved))
        })
        .collect()
}

/// Every option of every entry, each from the highest layer naming the
/// entry that sets it.
pub fn section_entries<T: Options>(
    prefix: &str,
    sources: &[(&BTreeMap<String, T>, Layer)],
    entries: &mut Vec<Entry>,
) -> Result<()> {
    let built_in = T::built_in();
    let names: std::collections::BTreeSet<&String> = sources
        .iter()
        .flat_map(|(sections, _)| sections.keys())
        .collect();
    for name in names {
        let mut layers: Vec<(&T, Layer)> = sources
            .iter()
            .filter_map(|(sections, layer)| Some((sections.get(name)?, *layer)))
            .collect();
        layers.push((&built_in, Layer::BuiltInDefault));
        T::entries(&key(prefix, name), &layers, entries)?;
    }
    Ok(())
}

/// One field of each source.
pub fn project<'a, S, T>(sources: &[(&'a S, Layer)], field: fn(&S) -> &T) -> Vec<(&'a T, Layer)> {
    sources
        .iter()
        .map(|(config, layer)| (field(config), *layer))
        .collect()
}

pub fn scalar_entry<T: Serialize>(
    key: String,
    sources: &[(&Option<T>, Layer)],
    entries: &mut Vec<Entry>,
) -> Result<()> {
    let found = sources
        .iter()
        .find_map(|(value, layer)| value.as_ref().map(|value| (value, *layer)));
    let (value, layer) = match found {
        Some((value, layer)) => (serde_json::to_value(value)?, layer),
        None => (Value::Null, Layer::BuiltInDefault),
    };
    entries.push(Entry {
        key,
        value,
        layer,
        file: None,
    });
    Ok(())
}

pub fn list_entry<T: Serialize>(
    key: String,
    sources: &[(&Vec<T>, Layer)],
    entries: &mut Vec<Entry>,
) -> Result<()> {
    let (value, layer) = sources
        .iter()
        .find(|(value, _)| !value.is_empty())
        .or(sources.last())
        .context("no configuration layers")?;
    entries.push(Entry {
        key,
        value: serde_json::to_value(value)?,
        layer: *layer,
        file: None,
    });
    Ok(())
}

/// The combined text, reported from the highest layer that adds to it.
pub fn template_entry(
    key: String,
    sources: &[(&Option<String>, Layer)],
    entries: &mut Vec<Entry>,
) -> Result<()> {
    let find = |repository: bool| {
        sources.iter().find_map(|(value, layer)| {
            (layer.is_repository() == repository)
                .then_some(value.as_ref())
                .flatten()
                .map(|value| (value, *layer))
        })
    };
    let base = find(false);
    let addition = find(true);
    entries.push(Entry {
        key,
        value: serde_json::to_value(append_template(
            base.map(|(value, _)| value.clone()),
            addition.map(|(value, _)| value.clone()),
        ))?,
        layer: addition
            .or(base)
            .map_or(Layer::BuiltInDefault, |(_, layer)| layer),
        file: None,
    });
    Ok(())
}

pub fn named_entries<T: Serialize>(
    prefix: &str,
    sources: &[(&BTreeMap<String, T>, Layer)],
    entries: &mut Vec<Entry>,
) -> Result<()> {
    for (name, (value, layer)) in winners(sources) {
        entries.push(Entry {
            key: key(prefix, name),
            value: serde_json::to_value(value)?,
            layer,
            file: None,
        });
    }
    Ok(())
}

/// Each name's entry from the highest layer that has one.
pub fn winners<'a, T>(
    sources: &[(&'a BTreeMap<String, T>, Layer)],
) -> BTreeMap<&'a String, (&'a T, Layer)> {
    let mut result = BTreeMap::new();
    for (table, layer) in sources.iter().rev() {
        for (name, value) in table.iter() {
            result.insert(name, (value, *layer));
        }
    }
    result
}

#[cfg(test)]
mod tests;
