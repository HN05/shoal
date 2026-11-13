//! The branches whose CI runs a scoped caller starts, reruns and cancels:
//! the default branch and the branches of the workspaces its scope reaches,
//! in the repository it acts in, and the branch a run is of.
use anyhow::{Context, Result, ensure};

use super::place::RunPlace;
use crate::{
    client::repository_ids::RepositoryId,
    daemon::{scope::Caller, workspace::Manager},
    forge::{
        ci::run_action::Run,
        items::{link::LinkKind, reference::ItemRef},
    },
    git::{self, default_branch::DefaultBranchLookup},
};

/// The branch of the place's repository `run` is of: the head branch of
/// the PR it ran for, whatever commit of the PR it ran on, since a forge
/// may name the PR as its branch, otherwise the branch it ran on. A fork's
/// PR has none there.
pub(super) async fn run_branch(place: &RunPlace, run: &Run) -> Result<Option<String>> {
    let Some(number) = run.pull else {
        return Ok(run.branch.clone());
    };
    let url = ItemRef::numbered(&place.forge, LinkKind::Pr, number.get())?;
    let pull = (place.forge.pull_request(&place.account, &url).await)
        .with_context(|| format!("could not read {url}, which {} ran for", run.url))?;
    Ok((!pull.fork).then(|| pull.head.to_string()))
}

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
