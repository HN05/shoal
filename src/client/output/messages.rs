//! What `shoal messages` prints.
use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// The workspaces a repository-wide message reached, by name.
#[derive(Serialize, Deserialize)]
pub struct Reached {
    pub workspaces: Vec<String>,
}

outputs!(Reached);
