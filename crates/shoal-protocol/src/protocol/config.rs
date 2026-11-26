//! Requests for a repository's config, and the layers they return.
use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;

/// Requests for the repository config of a space, a repository or the
/// global config alone.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigMethod {
    /// The config file the repository of the space `space` tracks, as
    /// TOML, which a machine without that repository reads a child's modes
    /// under. Answers with [`super::Body::TrackedConfig`].
    TrackedConfig { space: SpaceSelector },
    /// The repository layer of the target's config, for the CLI to resolve
    /// against the global config it reads at launch.
    LayeredConfig { target: ConfigTarget },
    /// The value of `key` for the space `space`, which a process there
    /// cannot read itself. Answers with [`super::Body::ConfigValue`].
    ConfigValue {
        space: SpaceSelector,
        key: crate::config::value::ValueKey,
    },
}

impl ConfigMethod {
    /// A space's config is read on its machine.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::TrackedConfig { .. } | Self::LayeredConfig { .. } => Forwarding::Online,
            // Asked by a process of this machine, whose daemon reads it.
            Self::ConfigValue { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::TrackedConfig { space }
            | Self::ConfigValue { space, .. }
            | Self::LayeredConfig {
                target: ConfigTarget::Space(space),
            } => Some(Target::Space(space)),
            Self::LayeredConfig { .. } => None,
        }
    }
}

/// Whose repository config: a space's, a workspace's worktree file under
/// the saved config or a chat's repository's; the registered checkout's
/// before a workspace exists; or none, for the global config alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigTarget {
    Space(crate::space_names::SpaceSelector),
    Repository(crate::repository::Selector),
    Global,
}

impl ConfigTarget {
    /// The config of `chat`: its repository's, or the global config alone
    /// for a chat without a repository.
    pub fn chat(chat: &crate::model::Chat) -> Self {
        match &chat.repository_id {
            Some(id) => Self::Repository(id.clone().into()),
            None => Self::Global,
        }
    }

    /// The config of `space`: a workspace's own, or a chat's.
    pub fn space(space: &crate::model::Space) -> Self {
        Self::Space(crate::space_names::SpaceSelector::id(&space.id))
    }
}

/// A target's repository config layers as the daemon read them. The
/// protocol carries them without reading them; `shoal-core` parses them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepositoryLayers(serde_json::Value);

impl RepositoryLayers {
    pub fn of(layers: &impl Serialize) -> Result<Self> {
        Ok(Self(serde_json::to_value(layers)?))
    }

    pub fn read<L: DeserializeOwned>(self) -> Result<L> {
        Ok(serde_json::from_value(self.0)?)
    }
}
