//! The branches whose CI runs a scoped caller starts, reruns and cancels:
//! the default branch and the branches of the workspaces its scope reaches,
//! in the repository it acts in.
use anyhow::{Context, Result, ensure};
use shoal_core::git::{self, default_branch::DefaultBranchLookup};
use shoal_daemon_base::callers::Caller;
use shoal_ids::repository_ids::RepositoryId;

use super::place::RunPlace;
use crate::{Links, daemon::Daemon};

impl Links {
    /// Refuse `caller` a run on `branch` in the repository of `place`,
    /// unless it is the repository's default branch or the branch of a
    /// workspace there that the caller's scope reaches; a run without a
    /// branch, as on a tag, is the user's.
    pub(super) async fn ensure_run_branch_in_reach(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        place: &RunPlace,
        branch: Option<&str>,
    ) -> Result<()> {
        let branch = branch.context("scoped processes act only on CI runs of a branch")?;
        let repository = place.repository_id().context(
            "scoped processes act only on CI runs of the repository of a space their scope reaches",
        )?;
        ensure!(
            branch_in_reach(daemon, caller, repository, branch).await?,
            "{branch} is neither the default branch nor the branch of a workspace in this space's scope"
        );
        Ok(())
    }
}

/// Whether `branch` of `repository` is its default branch or the branch of
/// a workspace there that `caller`'s scope reaches.
pub async fn branch_in_reach(
    daemon: &dyn Daemon,
    caller: &Caller,
    repository: &RepositoryId,
    branch: &str,
) -> Result<bool> {
    let workspaces = daemon.reached_workspaces(caller).await?;
    let reached = workspaces.iter().any(|workspace| {
        workspace.repository() == repository
            && workspace.branch().is_some_and(|own| own.as_str() == branch)
    });
    if reached {
        return Ok(true);
    }
    let checkout = daemon.repository(repository).await?;
    let default = git::default_branch::resolve(&checkout.path, DefaultBranchLookup::Cached);
    Ok(default.await?.as_str() == branch)
}
