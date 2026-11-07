//! What `shoal split` prints.
use serde::{Deserialize, Serialize};

use crate::{model::Workspace, output::outputs};

/// The ready child.
#[derive(Serialize, Deserialize)]
pub struct Child {
    #[serde(flatten)]
    pub workspace: Workspace,
}

outputs!(Child);
