//! The mode a start takes: named, or picked among the configured modes.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};

use super::{Entries, aligned, pick};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    config::modes::{self, Mode},
    model::SpaceKind,
};

/// The mode a start takes: the one `--mode` names, or one picked once the
/// start has checked the arguments it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeChoice {
    Named(String),
    /// A mode picked on the terminal; `forced` is the kind of space the
    /// command line asks for whatever the mode's.
    Pick {
        forced: Option<SpaceKind>,
    },
}

impl ModeChoice {
    /// The mode of a start in `kind` of space, or in either when `None`:
    /// the named one, which `modes` must define, else one picked among
    /// those that start there, without asking when only one does.
    pub fn chosen(
        self,
        ctx: &Context,
        modes: &BTreeMap<String, Mode>,
        kind: Option<SpaceKind>,
    ) -> Result<String> {
        let forced = match self {
            Self::Named(name) => {
                modes::get(modes, &name)?;
                return Ok(name);
            }
            Self::Pick { forced } => forced,
        };
        let entries = mode_entries(modes, kind, forced, Palette::picker());
        match entries.as_slice() {
            [(only, _)] => Ok(only.clone()),
            _ => {
                let kind = kind.map_or_else(|| "space".to_owned(), |kind| kind.to_string());
                ensure!(!entries.is_empty(), "no mode starts in a {kind}");
                pick(ctx, "Mode> ", entries)
            }
        }
    }
}

/// The picker's entries for the `modes` that start in `kind` of space,
/// `forced` or else their own: the usual mode of that kind first, so Enter
/// keeps it, then the rest by name, each with its kind of space.
fn mode_entries(
    modes: &BTreeMap<String, Mode>,
    kind: Option<SpaceKind>,
    forced: Option<SpaceKind>,
    palette: Palette,
) -> Entries {
    let usual = match forced.or(kind) {
        Some(SpaceKind::Chat) => modes::CHAT,
        _ => modes::DEFAULT,
    };
    let mut modes: Vec<_> = (modes.iter())
        .map(|(name, mode)| (name, forced.unwrap_or(mode.kind)))
        .filter(|(_, starts_in)| kind.is_none_or(|kind| kind == *starts_in))
        .collect();
    modes.sort_by_key(|(name, _)| name.as_str() != usual);
    let rows = modes
        .iter()
        .map(|(name, starts_in)| {
            [
                ((*name).clone(), None),
                (starts_in.to_string(), Some(Style::Muted)),
            ]
        })
        .collect();
    let labels = aligned(rows, palette);
    (modes.into_iter())
        .map(|(name, _)| name.clone())
        .zip(labels)
        .collect()
}

#[cfg(test)]
mod tests;
