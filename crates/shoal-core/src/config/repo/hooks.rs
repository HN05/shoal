//! The `[hooks]` section: the commands lifecycle events run.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Hooks)]
pub struct HooksConfig {
    pub pre_setup: Option<String>,
    pub post_setup: Option<String>,
    pub pre_remove: Option<String>,
    pub post_remove: Option<String>,
    pub post_done: Option<String>,
    pub post_ready: Option<String>,
    pub post_agent_exit: Option<String>,
    pub post_resource_acquire: Option<String>,
    pub pre_resource_release: Option<String>,
}
