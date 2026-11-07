//! What `shoal cleanup` prints.
use serde::{Deserialize, Serialize};

use crate::{cleanup::ManualCleanup, model::KeptBranch, output::outputs};

/// What a cleanup removed, or would remove in a dry run.
#[derive(Serialize, Deserialize)]
pub struct Report {
    #[serde(flatten)]
    pub cleanup: ManualCleanup,
    /// Kept branches: those a dry run lists, or those deleted.
    pub branches: Vec<KeptBranch>,
}

outputs!(Report);
