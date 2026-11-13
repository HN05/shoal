//! The branches whose CI runs a scoped caller starts, reruns and cancels:
//! the default branch and the branches of the workspaces its scope reaches,
//! in the repository it acts in.
use anyhow::{Context, Result, ensure};

use super::place::RunPlace;
use crate::{
    client::repository_ids::RepositoryId,
    daemon::{scope::Caller, workspace::Manager},
    git::{self, default_branch::DefaultBranchLookup},
};

impl Manager {
    /// Refuse `caller` a run on `branch` in the repository of `place`,
    /// unless it is the repository's default branch or the branch of a
    /// workspace there that the caller's scope reaches; a run without a
    /// branch, as on a tag, is the user's.
    pub(super) async fn ensure_run_branch_in_reach(
        &self,
        caller: &Caller,
        place: &RunPlace,
        branch: Option<&str>,
    ) -> Result<()> {
        let branch = branch.context("scoped processes act only on CI runs of a branch")?;
        let repository = place.repository_id().context(
            "scoped processes act only on CI runs of the repository of a space their scope reaches",
        )?;
        ensure!(
            self.branch_in_reach(caller, repository, branch).await?,
            "{branch} is neither the default branch nor the branch of a workspace in this space's scope"
        );
        Ok(())
    }

    /// Whether `branch` of `repository` is its default branch or the branch
    /// of a workspace there that `caller`'s scope reaches.
    async fn branch_in_reach(
        &self,
        caller: &Caller,
        repository: &RepositoryId,
        branch: &str,
    ) -> Result<bool> {
        let root = self.reach_root(Some(caller)).await?;
        let workspaces = self.list_workspaces_within(root.as_ref()).await?;
        let reached = workspaces.iter().any(|workspace| {
            workspace.repository_id == *repository
                && workspace
                    .branch
                    .as_ref()
                    .is_some_and(|own| own.as_str() == branch)
        });
        if reached {
            return Ok(true);
        }
        let checkout = self.repository(repository).await?;
        let default = git::default_branch::resolve(&checkout.path, DefaultBranchLookup::Cached);
        Ok(default.await?.as_str() == branch)
    }
}

#[cfg(test)]
mod tests;
