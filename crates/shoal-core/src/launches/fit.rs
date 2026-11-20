//! The modes a launch may take whatever labels say: those that start the
//! kind of space its arguments other than the mode fit, in a repository
//! where they call for one.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::{fitting_kind, named_issue, names_repository};
use crate::{config::modes::ModeShape, model::SpaceKind};

/// The spaces a mode must start: of `kind`, and in a repository or not as
/// `repository` says; either when `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModeFit {
    pub kind: Option<SpaceKind>,
    pub repository: Option<bool>,
}

impl ModeFit {
    /// Whether a mode that starts the space `shape` describes fits.
    pub fn admits(&self, shape: &ModeShape) -> bool {
        self.kind.is_none_or(|kind| kind == shape.kind)
            && self
                .repository
                .is_none_or(|repository| repository == shape.repository)
    }
}

/// The spaces a mode of `launch` must start: of the kind its arguments
/// other than the mode fit, and in a repository where they
/// [require](requires_repository) one.
pub fn fit(launch: &Launch) -> Result<ModeFit> {
    Ok(ModeFit {
        kind: fitting_kind(launch),
        repository: requires_repository(launch)?.then_some(true),
    })
}

/// Whether the arguments of `launch` call for a repository whatever its
/// mode: they name one, an existing workspace, an issue, which only a
/// space in a repository starts on, or what only a workspace takes.
pub fn requires_repository(launch: &Launch) -> Result<bool> {
    Ok(launch.in_workspace.is_some()
        || launch.issue.is_some()
        || fitting_kind(launch).is_some()
        || names_repository(launch)?
        || named_issue(launch)?.is_some())
}

#[cfg(test)]
mod tests;
