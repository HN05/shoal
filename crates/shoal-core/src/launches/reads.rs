//! What resolving a launch reads of Shoal's state: the CLI asks the
//! daemon, which reads its own.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::Effective,
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

    /// The repository of the space `space` names, none for a chat without
    /// one.
    async fn space_repository(&self, space: &SpaceSelector) -> Result<Option<RepositoryId>>;

    /// The settings of `target`, with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    async fn settings(&self, target: ConfigTarget) -> Result<Effective>;
}
