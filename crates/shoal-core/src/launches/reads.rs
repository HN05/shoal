//! What resolving a launch reads of Shoal's state, which its caller reads
//! first: the CLI asks the daemon, which reads its own.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;

use crate::model::{Repository, Workspace};

/// What resolving a launch reads of Shoal's state, for its caller.
#[derive(Debug, Clone)]
pub struct LaunchState {
    /// The registered repositories.
    pub repositories: Vec<Repository>,
    /// The workspaces the caller sees.
    pub workspaces: Vec<Workspace>,
    /// The repository of the space the launch goes below, which a child
    /// shares; [`SpaceRepository::Without`] without one.
    pub parent: SpaceRepository,
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
