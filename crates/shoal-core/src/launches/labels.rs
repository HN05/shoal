//! The mode the labels of the issue or PR a launch names call for, among
//! the modes that start the spaces its arguments other than the mode fit,
//! which the daemon decides for a launch that names no mode.
use anyhow::Result;
use shoal_protocol::launches::{LabelledMode, Launch, SeveralModes};

use super::{fitting_kind, named_issue, named_pull, names_repository};
use crate::{
    config::modes::{ModeName, ModeShape, ModeShapes},
    model::SpaceKind,
};

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

/// Whether `launch` names the issue or PR whose labels may call for its
/// mode.
pub fn names_item(launch: &Launch) -> Result<bool> {
    Ok(named_pull(launch).is_some() || named_issue(launch)?.is_some())
}

/// Each of `modes` with a label, by name with its label, that starts the
/// spaces `launch` [fits](fit).
pub fn labelled<'a>(launch: &Launch, modes: &'a ModeShapes) -> Result<Vec<(&'a str, &'a str)>> {
    let fit = fit(launch)?;
    Ok((modes.iter())
        .filter(|(_, shape)| fit.admits(shape))
        .filter_map(|(name, shape)| Some((name.as_str(), shape.label.as_deref()?)))
        .collect())
}

/// The mode the `labels` of the issue or PR `item` names call for among
/// `modes`, each by name with the label it has: the one mode whose label
/// it carries, or the several whose labels it carries.
pub fn labelled_mode<'a>(
    item: String,
    modes: impl IntoIterator<Item = (&'a str, &'a str)>,
    labels: &[String],
) -> LabelledMode {
    let mut called: Vec<_> = (modes.into_iter())
        .filter(|(_, label)| labels.iter().any(|carried| carried == label))
        .map(|(mode, label)| (ModeName::from(mode), label.to_owned()))
        .collect();
    match called.len() {
        0 => LabelledMode::Unlabelled,
        1 => LabelledMode::Called(called.remove(0).0),
        _ => LabelledMode::Several(SeveralModes {
            item,
            modes: called,
        }),
    }
}

#[cfg(test)]
mod tests;
