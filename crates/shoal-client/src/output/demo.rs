//! What `shoal demo` prints.
use serde::{Deserialize, Serialize};
use shoal_protocol::space_names::SpaceName;

use std::collections::BTreeMap;

use crate::output::outputs;
use shoal_protocol::demos::{Demo, Running};

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

/// A demo another machine runs, which `shoal demo` forwards from a port
/// of this machine until it stops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forwarding {
    pub name: String,
    /// The machine that runs it.
    pub machine: String,
    /// Where the user reaches it here: a web demo's URL, otherwise its
    /// address.
    pub location: String,
    /// The port of this machine's loopback it listens on.
    pub port: u16,
}

outputs!(Stopping, Screen, Sent, Forwarding);
