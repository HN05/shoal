//! The `[pr]` section.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Pr)]
pub struct PrConfig {
    #[option(default = false)]
    pub draft: Option<bool>,
    #[option(default = true)]
    pub undraft_on_ready: Option<bool>,
    #[option(default = false)]
    pub draft_on_unready: Option<bool>,
    #[option(default = Vec::new())]
    pub required_labels: Option<Vec<String>>,
    #[option(default = false)]
    pub progress: Option<bool>,
    #[option(default = false)]
    pub watch_merge_runs: Option<bool>,
}
