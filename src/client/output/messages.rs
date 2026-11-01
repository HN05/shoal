//! What `shoal messages` prints.
use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// The workspaces and chats a repository-wide message reached, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    pub workspaces: Vec<String>,
    pub chats: Vec<String>,
}

outputs!(Reached);
