//! The `[priority]` section: priorities that labels give.
use crate::config::options::Options;
use shoal_schedule::priority::Priority;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = PrioritySettings)]
pub struct PriorityConfig {
    #[option(named)]
    pub labels: BTreeMap<String, Priority>,
}

impl PrioritySettings {
    pub fn of_labels<'a>(&self, labels: impl IntoIterator<Item = &'a str>) -> Option<Priority> {
        (labels.into_iter())
            .filter_map(|label| self.labels.get(label).copied())
            .max()
    }
}
