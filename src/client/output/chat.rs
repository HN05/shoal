//! What chat commands print.
use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// A chat started in a background place.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackgroundChat {
    pub chat: String,
    pub agent: String,
    pub repository: String,
    /// Where it runs and how to reach it, as the host that runs it says.
    pub place: String,
}

outputs!(BackgroundChat);
