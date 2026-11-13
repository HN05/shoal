//! The repository config's types that requests, records and output carry.
use serde::{Deserialize, Serialize};

use shoal_states::states;

/// Display uses human-readable layer labels rather than the snake_case JSON names.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    BuiltInDefault,
    GlobalConfig,
    WorktreeFile,
    LocalConfig,
}

impl ConfigLayer {
    /// Whether the repository sets this layer, rather than the machine.
    pub fn is_repository(self) -> bool {
        matches!(self, Self::WorktreeFile | Self::LocalConfig)
    }
}

impl std::fmt::Display for ConfigLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BuiltInDefault => "built-in default",
            Self::GlobalConfig => "global config",
            Self::WorktreeFile => "worktree file",
            Self::LocalConfig => "local config",
        })
    }
}

states!(
    #[derive(Default)]
    ConflictPolicy: ValueEnum {
        Auto => "auto",
        #[default]
        Suggest => "suggest",
    }
);

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PortDefinition {
    pub requires_approval: bool,
    pub approval_lifetime: crate::approvals::Lifetime,
    pub port: Option<u16>,
    pub env: Option<String>,
    pub reason: Option<String>,
    pub on_conflict: Option<ConflictPolicy>,
}
