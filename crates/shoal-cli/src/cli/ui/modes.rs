//! The mode a start takes: named, or picked among the configured modes.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};

use super::{Entries, aligned, pick};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    config::modes::{self, Mode, ModeName},
    model::SpaceKind,
};

/// The mode a start takes: the one `--mode` names, or one picked once the
/// start has checked the arguments it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeChoice {
    Named(ModeName),
    /// A mode picked on the terminal.
    Pick,
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
    ) -> Result<ModeName> {
        if let Self::Named(name) = self {
            modes::get(modes, &name)?;
            return Ok(name);
        }
        let entries = mode_entries(modes, fit, Palette::picker());
        match entries.as_slice() {
            [(only, _)] => Ok(only.as_str().into()),
            _ => {
                ensure!(!entries.is_empty(), "no mode starts this kind of space");
                pick(ctx, "Mode> ", entries).map(ModeName::from)
            }
        }
    }
}

/// The picker's entries for the `modes` that start in the spaces `fit`
/// allows: the usual mode of that kind first, so Enter keeps it, then the
/// rest by name, each with the space it starts.
fn mode_entries(modes: &BTreeMap<String, Mode>, fit: ModeFit, palette: Palette) -> Entries {
    let usual = match fit.kind {
        Some(SpaceKind::Chat) => ModeName::CHAT,
        _ => ModeName::DEFAULT,
    };
    let mut modes: Vec<_> = (modes.iter())
        .map(|(name, mode)| (name, mode.kind, mode.repository))
        .filter(|(_, kind, repository)| {
            fit.kind.is_none_or(|fit| fit == *kind)
                && fit.repository.is_none_or(|fit| fit == *repository)
        })
        .collect();
    modes.sort_by_key(|(name, _, _)| usual != name.as_str());
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
