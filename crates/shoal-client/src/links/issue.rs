//! The issue a workspace is assigned.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Registration {
    pub url: String,
    pub error: Option<String>,
}
