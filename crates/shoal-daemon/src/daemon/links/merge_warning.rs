//! What merging a workspace's branch locally warns about: the sensitive
//! files that its last conflict check found changed on both sides, read as
//! a PR's merge reads its workspaces' records.
use anyhow::Result;
use shoal_links::merge_warning::{Merged, warning};

use crate::{
    daemon::manager::Manager,
    git::{self, default_branch::DefaultBranchLookup},
    model::MergeWarning,
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The warning for merging the workspace's HEAD into the default branch,
    /// as its last check against that branch found it; a stacked
    /// workspace's check is against its base's branch instead.
    pub async fn branch_merge_warning(
        &self,
        selector: &SpaceSelector,
    ) -> Result<Option<MergeWarning>> {
        let workspace = self.workspace(selector).await?;
        let recorded = self.recorded_changed_on_both_sides(&workspace.id).await?;
        let Some(recorded) = recorded.filter(|recorded| !recorded.files.is_empty()) else {
            return Ok(None);
        };
        let repo = self.repositories.repository(workspace.repository()).await?;
        let target = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let merged = Merged {
            head,
            target: target.into(),
        };
        let branch = workspace.require_branch("merging")?.to_owned();
        Ok(warning(branch.into(), vec![recorded], &merged))
    }
}

#[cfg(test)]
mod tests;
