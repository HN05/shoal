//! The mode the labels of the issue or PR a launch names call for, which
//! the daemon decides for a launch that names no mode.
use serde::{Deserialize, Serialize};

use crate::config::modes::ModeName;

/// The mode the labels of the issue or PR a launch names call for, among
/// the modes that fit its other arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelledMode {
    /// The one mode whose label it carries.
    Called(ModeName),
    /// The several modes whose labels it carries, which no default
    /// settles.
    Several(SeveralModes),
    /// It names no issue or PR, or one that carries no mode's label.
    Unlabelled,
}

/// The modes, each with its label, whose labels the issue or PR `item`
/// carries, as a refusal names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeveralModes {
    pub item: String,
    pub modes: Vec<(ModeName, String)>,
}

impl std::fmt::Display for SeveralModes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let modes: Vec<_> = (self.modes.iter())
            .map(|(mode, label)| format!("{mode} (label {label})"))
            .collect();
        write!(
            f,
            "{}: its labels call for modes {}; name the mode or drop all but one label",
            self.item,
            modes.join(" and ")
        )
    }
}

impl std::error::Error for SeveralModes {}
