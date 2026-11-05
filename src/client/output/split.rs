//! What `shoal split` prints.
use serde::{Deserialize, Serialize};

use crate::{client::output::outputs, model::Workspace};

/// The ready child.
#[derive(Serialize, Deserialize)]
pub struct Child {
    #[serde(flatten)]
    pub workspace: Workspace,
}

outputs!(Child);
