//! What `shoal demo` prints.
use crate::space_names::SpaceName;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::{
    demos::{Demo, Running},
    output::outputs,
};

/// Configured and running demos, as `shoal demo ls --json` prints them.
#[derive(Serialize, Deserialize)]
pub struct Listing {
    pub workspace: Option<SpaceName>,
    pub configured: BTreeMap<String, Demo>,
    pub running: Vec<Running>,
}

outputs!(Listing);

/// A demo told to stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stopping {
    pub stopping: bool,
    /// The demo, or none for every demo of the workspace.
    pub name: Option<String>,
}

/// What a demo shows now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screen {
    pub name: String,
    pub screen: String,
}

/// Keys typed into a demo.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sent {
    pub sent: bool,
}

outputs!(Stopping, Screen, Sent);
