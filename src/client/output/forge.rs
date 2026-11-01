//! What `shoal forge` prints.
use serde::{Deserialize, Serialize};

use crate::client::output::outputs;

/// The forge's JSON response to `shoal forge api`, as it sent it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiResponse(pub serde_json::Value);

outputs!(ApiResponse);
