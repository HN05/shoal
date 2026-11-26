//! The `[alerts]` section.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Alerts)]
pub struct AlertsConfig {
    #[option(default = 5)]
    pub settle_seconds: Option<u64>,
}

impl AlertsConfig {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if let Some(seconds) = self.settle_seconds {
            anyhow::ensure!(seconds <= 60, "alerts.settle_seconds must be at most 60");
        }
        Ok(())
    }
}
