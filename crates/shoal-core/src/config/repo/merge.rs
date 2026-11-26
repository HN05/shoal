//! The `[merge]` section.
use crate::config::options::Options;
use shoal_forge_types::items::action::MergeMethod;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Merge)]
pub struct MergeConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = true)]
    pub confirm: Option<bool>,
    #[option(default = false)]
    pub push: Option<bool>,
    pub method: Option<MergeMethod>,
}
