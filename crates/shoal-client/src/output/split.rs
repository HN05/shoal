//! What `shoal split` prints.
use serde::{Deserialize, Serialize};

use crate::output::outputs;
use shoal_protocol::model::Workspace;

/// The ready child.
#[derive(Serialize, Deserialize)]
pub struct Child {
    #[serde(flatten)]
    pub workspace: Workspace,
}

outputs!(Child);
