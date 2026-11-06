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

/// The spaces a picked mode must start: of `kind`, and in a repository or
/// not as `repository` says; either when `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModeFit {
    pub kind: Option<SpaceKind>,
    pub repository: Option<bool>,
}

impl ModeFit {
    /// A space of `kind` in a repository.
    pub fn in_repository(kind: SpaceKind) -> Self {
        Self {
            kind: Some(kind),
            repository: Some(true),
        }
    }
}

impl ModeChoice {
    /// The mode of a start in the spaces `fit` allows: the named one, which
    /// `modes` must define, else one picked among those that start there,
    /// without asking when only one does.
    pub fn chosen(
        self,
        ctx: &Context,
        modes: &BTreeMap<String, Mode>,
        fit: ModeFit,
    ) -> Result<String> {
        let forced = match self {
            Self::Named(name) => {
                modes::get(modes, &name)?;
                return Ok(name);
            }
            Self::Pick { forced } => forced,
        };
        let entries = mode_entries(modes, fit, forced, Palette::picker());
        match entries.as_slice() {
            [(only, _)] => Ok(only.clone()),
            _ => {
                ensure!(!entries.is_empty(), "no mode starts this kind of space");
                pick(ctx, "Mode> ", entries)
            }
        }
    }
}

/// The picker's entries for the `modes` that start in the spaces `fit`
/// allows, of the kind `forced` or else their own: the usual mode of that
/// kind first, so Enter keeps it, then the rest by name, each with the
/// space it starts.
fn mode_entries(
    modes: &BTreeMap<String, Mode>,
    fit: ModeFit,
    forced: Option<SpaceKind>,
    palette: Palette,
) -> Entries {
    let usual = match forced.or(fit.kind) {
        Some(SpaceKind::Chat) => modes::CHAT,
        _ => modes::DEFAULT,
    };
    let mut modes: Vec<_> = (modes.iter())
        .map(|(name, mode)| (name, forced.unwrap_or(mode.kind), mode.repository))
        .filter(|(_, kind, repository)| {
            fit.kind.is_none_or(|fit| fit == *kind)
                && fit.repository.is_none_or(|fit| fit == *repository)
        })
        .collect();
    modes.sort_by_key(|(name, _, _)| name.as_str() != usual);
    let rows = modes
        .iter()
        .map(|(name, kind, repository)| {
            let space = match repository {
                true => kind.to_string(),
                false => format!("{kind} without a repository"),
            };
            [((*name).clone(), None), (space, Some(Style::Muted))]
        })
        .collect();
    let labels = aligned(rows, palette);
    (modes.into_iter())
        .map(|(name, _, _)| name.clone())
        .zip(labels)
        .collect()
}

#[cfg(test)]
mod tests;
