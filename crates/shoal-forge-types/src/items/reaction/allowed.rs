//! The reactions a repository's forge allows, so a client can offer them
//! before it reacts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// GitHub's reactions, which every forge with reactions allows by default.
const GITHUB: [&str; 8] = [
    "+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes",
];

/// The reactions a forge allows, under the names `shoal view` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedReactions {
    pub names: Vec<String>,
    /// Whether the forge also accepts names it does not list, which makes
    /// `names` suggestions.
    pub accepts_other_names: bool,
}

impl AllowedReactions {
    /// GitHub's reactions, as the only ones or as suggestions.
    pub fn github(accepts_other_names: bool) -> Self {
        Self {
            names: GITHUB.map(str::to_owned).to_vec(),
            accepts_other_names,
        }
    }

    /// Exactly the names in `list`, a JSON array of them, which Forgejo
    /// reports as `null` when empty.
    pub fn only(list: &Value) -> Result<Self> {
        let names = match list {
            Value::Null => Vec::new(),
            list => serde_json::from_value(list.clone()).context("read the allowed reactions")?,
        };
        Ok(Self {
            names,
            accepts_other_names: false,
        })
    }
}
