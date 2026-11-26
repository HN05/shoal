//! The `[ports]` section: configured ports and the automatic range.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};
pub(crate) use shoal_protocol::config::repo::{ConflictPolicy, PortDefinition};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default)]
#[options(resolved = Ports)]
pub struct PortsConfig {
    #[option(default = ConflictPolicy::default())]
    pub on_conflict: Option<ConflictPolicy>,
    /// Bounds of the automatic range.
    #[option(default = 49152)]
    pub start: Option<u16>,
    #[option(default = 65535)]
    pub end: Option<u16>,
    #[serde(flatten)]
    #[option(named)]
    pub definitions: BTreeMap<String, PortDefinition>,
}
