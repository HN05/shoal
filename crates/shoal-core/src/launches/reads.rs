//! What resolving a launch reads of Shoal's state: the CLI asks the
//! daemon, which reads its own.
use anyhow::Result;

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

    /// The settings of `target`, with the mode and rule files beside the
    /// global config, which a launch's mode may come from.
    async fn settings(&self, target: ConfigTarget) -> Result<Effective>;

    /// Refuse what opening the existing branch `existing` in the repository
    /// `repository` names would refuse, such as a local branch of its name
    /// that does not track it.
    async fn check_opens(
        &self,
        repository: &crate::repository::Selector,
        existing: &str,
    ) -> Result<()>;
}
