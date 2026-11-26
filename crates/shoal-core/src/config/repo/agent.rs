//! The `[agent]` section: the agent, model and directories launches use.
use crate::agent::Agent;
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = AgentSettings)]
pub struct AgentConfig {
    #[serde(deserialize_with = "crate::agent::configured")]
    pub default: Option<Agent>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[option(default = crate::config::RunLimit::Dynamic)]
    pub max_heavy_running: Option<crate::config::RunLimit>,
    #[option(default = vec!["/tmp".into()])]
    pub dirs: Option<Vec<String>>,
    pub git_profile: Option<String>,
}
