//! What resolving a launch reads of Shoal's state: the CLI asks the
//! daemon, which reads its own.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::{Effective, modes::ModeShapes},
    model::{Repository, Workspace},
    protocol::ConfigTarget,
};

/// What resolving a launch reads of Shoal's state, for its caller.
// Only Shoal's own crates implement and await it.
#[allow(async_fn_in_trait)]
pub trait LaunchReads {
    /// The registered repositories.
    async fn repositories(&self) -> Result<Vec<Repository>>;

    /// The workspaces the caller sees.
    async fn workspaces(&self) -> Result<Vec<Workspace>>;

    /// The repository of the space `space` names.
    async fn space_repository(&self, space: &SpaceSelector) -> Result<SpaceRepository>;

    /// The settings of `target`, with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    async fn settings(&self, target: ConfigTarget) -> Result<Effective>;

    /// The modes `launch` may start in, each with the space it starts, as
    /// the machine it starts on defines them where
    /// [`super::shape::launch_settings`] would find them there.
    async fn launch_modes(&self, launch: &Launch) -> Result<ModeShapes>;
}

/// The repository of a space as this machine knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceRepository {
    /// A chat without a repository.
    Without,
    /// This machine's registration of its repository.
    Registered(RepositoryId),
    /// The repository of another joined machine's space, which this
    /// machine has not registered.
    Unregistered,
}

impl SpaceRepository {
    /// This machine's registration of the repository, if it has one.
    pub fn registered(self) -> Option<RepositoryId> {
        match self {
            Self::Registered(id) => Some(id),
            Self::Without | Self::Unregistered => None,
        }
    }

    /// This machine's registration of the repository, none for a chat
    /// without one; refused for a repository this machine has not
    /// registered, which no other one stands in for.
    pub fn here(self) -> Result<Option<RepositoryId>> {
        match self {
            Self::Registered(id) => Ok(Some(id)),
            Self::Without => Ok(None),
            Self::Unregistered => anyhow::bail!(
                "the space this start goes below runs on another machine, in a repository this machine has not registered"
            ),
        }
    }
}
