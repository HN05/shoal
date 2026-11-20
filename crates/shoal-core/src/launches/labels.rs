//! The mode the labels of the issue or PR a launch names call for, among
//! the modes that start the spaces its arguments other than the mode fit,
//! which the daemon decides for a launch that names no mode.
use anyhow::Result;
use shoal_protocol::launches::{LabelledMode, Launch, SeveralModes};

use super::{fit::fit, named_issue, named_pull};
use crate::config::modes::{ModeName, ModeShapes};

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
