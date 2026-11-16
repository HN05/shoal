//! An edit of a config file, which a request carries.
use serde::{Deserialize, Serialize};

/// One key to set, or to remove when `value` is `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub key: String,
    pub value: Option<String>,
}
