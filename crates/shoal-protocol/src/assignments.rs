//! A space's assignment: what marking it done records.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::files::NewFile;

/// What `shoal done` records besides the HEAD it binds to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    /// Remove the space once nothing keeps it, even when `[cleanup.done]
    /// enabled` is false.
    pub cleanup: bool,
    /// Another branch has HEAD's work under other commits; only a
    /// workspace has such a branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub integrated: bool,
    /// Leave the workspace's demos running; a chat has none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_demos: bool,
    /// Files to hand back by absolute path, which the daemon copies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<NewFile>,
    /// One line saying what the work found, which the completion notice
    /// ends with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Done {
    /// Refuse what one completion cannot record, before it has any effect.
    pub fn check(&self) -> Result<()> {
        crate::files::check_results(&self.results)?;
        if let Some(summary) = &self.summary {
            crate::validate::message(summary).context("invalid summary")?;
        }
        Ok(())
    }
}
