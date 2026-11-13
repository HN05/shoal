//! Whose repository config a request reads, and the layers it returns.
use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Whose repository config: a workspace's worktree file under the saved
/// config, the registered checkout's before a workspace exists, or none,
/// for the global config alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigTarget {
    Workspace(crate::space_names::SpaceSelector),
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
    pub fn space(space: &crate::model::PresentSpace) -> Self {
        match space {
            crate::model::PresentSpace::Workspace(workspace) => {
                Self::Workspace(crate::space_names::SpaceSelector::id(&workspace.id))
            }
            crate::model::PresentSpace::Chat(chat) => Self::chat(chat),
        }
    }
}

/// A target's repository config layers as the daemon read them, in the
/// schema of the config module of Shoal's own code, which the client carries
/// without reading: launch settings come from the daemon, and the `shoal`
/// command resolves the layers under the global config it reads itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepositoryLayers(serde_json::Value);

impl RepositoryLayers {
    /// `layers` as the daemon sends them.
    pub fn of(layers: &impl Serialize) -> Result<Self> {
        Ok(Self(serde_json::to_value(layers)?))
    }

    /// The layers, read as `L`.
    pub fn read<L: DeserializeOwned>(self) -> Result<L> {
        Ok(serde_json::from_value(self.0)?)
    }
}
