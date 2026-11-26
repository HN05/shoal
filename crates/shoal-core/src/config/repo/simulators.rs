//! The `[simulators]` section.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Simulators)]
pub struct SimulatorsConfig {
    #[option(default = false)]
    pub requires_approval: Option<bool>,
    #[option(default = shoal_protocol::config::repo::Lifetime::default())]
    pub approval_lifetime: Option<shoal_protocol::config::repo::Lifetime>,
    #[option(list)]
    pub preferred: Vec<String>,
}
