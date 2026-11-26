//! The `[sleep]` section: stopping idle agents.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = SleepSettings)]
pub struct SleepConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = 30)]
    pub max_wait_minutes: Option<u64>,
    #[option(default = 30)]
    pub keep_leases_minutes: Option<u64>,
}
