//! The `[runner]` section: the CI runner the machine hosts.
use crate::config::options::Options;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Runner)]
pub struct RunnerConfig {
    #[option(default = false)]
    pub enabled: Option<bool>,
    #[option(default = vec!["shoal".into()])]
    pub labels: Option<Vec<String>>,
    pub permit: Option<String>,
}

impl RunnerConfig {
    pub(super) fn validate(&self) -> Result<()> {
        for label in self.labels.iter().flatten() {
            ensure!(
                !label.is_empty()
                    && !label
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, ',' | ':')),
                "runner.labels entries must be non-empty, without spaces, commas or colons: {label:?}"
            );
        }
        Ok(())
    }
}
