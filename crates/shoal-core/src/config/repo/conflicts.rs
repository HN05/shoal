//! The `[conflicts]` section.
use crate::config::options::Options;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Conflicts)]
pub struct ConflictsConfig {
    #[option(default = Vec::new())]
    pub sensitive: Option<Vec<String>>,
}

impl ConflictsConfig {
    pub fn validate(&self) -> Result<()> {
        for pattern in self.sensitive.iter().flatten() {
            ensure!(
                !pattern.is_empty()
                    && !pattern.starts_with(['/', ':'])
                    && !pattern.split('/').any(|part| part == ".."),
                "conflicts.sensitive entries must be paths or globs relative to the repository root: {pattern}"
            );
        }
        Ok(())
    }
}
