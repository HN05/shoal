//! What commands that act on several spaces print of those they failed on.
use serde::{Deserialize, Serialize};

/// A space a command failed on, which did not stop it acting on the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub workspace: String,
    pub error: String,
}
