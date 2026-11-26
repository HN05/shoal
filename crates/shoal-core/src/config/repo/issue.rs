//! The `[issue]` section: required labels, assignment and areas.
use crate::config::options::Options;
use shoal_states::states;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Issue)]
pub struct IssueConfig {
    #[option(default = Vec::new())]
    pub required_labels: Option<Vec<String>>,
    #[option(default = IssueAssign::default())]
    pub assign: Option<IssueAssign>,
    pub(crate) area_prefix: Option<String>,
}

states!(
    #[derive(Default)]
    IssueAssign {
        #[default]
        Agent => "agent",
        User => "user",
        Both => "both",
        Nobody => "none",
    }
);

impl IssueAssign {
    pub fn agent(self) -> bool {
        matches!(self, Self::Agent | Self::Both)
    }

    pub fn user(self) -> bool {
        matches!(self, Self::User | Self::Both)
    }
}

impl Issue {
    /// The areas among `labels`: those with the area prefix, none while it
    /// is unset.
    pub fn areas<'a>(&self, labels: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let Some(prefix) = &self.area_prefix else {
            return Vec::new();
        };
        let mut areas: Vec<String> = (labels.into_iter())
            .filter(|label| label.starts_with(prefix.as_str()))
            .map(String::from)
            .collect();
        areas.sort();
        areas.dedup();
        areas
    }
}
