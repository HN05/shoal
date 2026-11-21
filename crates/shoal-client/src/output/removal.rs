//! What commands that remove spaces print: `shoal rm` and `shoal absorb`.
use shoal_protocol::space_names::SpaceName;

use serde::{Deserialize, Serialize};

use crate::output::{batch::Failure, outputs};
use shoal_protocol::{model::SpaceHold, removal::RemovalResult};

/// A removed space, or one being removed, and the holds it had.
#[derive(Debug, Serialize, Deserialize)]
pub struct Removed {
    /// The space's name, which a removal of several reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceName>,
    /// What the removal did, once it finished.
    #[serde(flatten)]
    pub removal: Option<RemovalResult>,
    /// The removal goes on in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundRemoval>,
    pub holds: Vec<SpaceHold>,
    /// Git no longer knew its worktree, so the removal deleted what was
    /// left of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disowned: bool,
}

impl Removed {
    /// The spaces below it the removal took, or takes, first.
    pub fn children(&self) -> &[SpaceName] {
        match (&self.removal, &self.background) {
            (Some(removal), _) => &removal.children,
            (None, Some(background)) => &background.children,
            (None, None) => &[],
        }
    }
}

/// A removal that goes on after the command returned.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackgroundRemoval {
    /// The spaces below it it takes first, each after those below it.
    pub children: Vec<SpaceName>,
}

/// The spaces a removal of several removed.
#[derive(Debug, Serialize, Deserialize)]
pub struct Removals {
    pub removed: Vec<Removed>,
    pub failed: Vec<Failure>,
}

outputs!(shoal_protocol::absorb::Absorbed, Removed, Removals);
