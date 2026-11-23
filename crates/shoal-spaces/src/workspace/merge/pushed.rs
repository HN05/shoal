//! Following the default branch's CI runs for the commit a merge pushed,
//! through the lookup that follows a merged PR's.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::PushedMerge;
use anyhow::Result;

use super::{RecordChange, tracking_ref};
use shoal_refs::CommitId;

use crate::{git, log, model::MergePlan};

impl Spaces {
    /// Follow the default branch's CI runs for the commit the execution
    /// behind `change` pushed, if it merged and pushed. The caller holds the
    /// repository Git gate. Failure only logs, so the execution's outcome
    /// stands.
    pub async fn follow_pushed_merge_runs(&self, daemon: &dyn Daemon, change: &RecordChange) {
        let RecordChange::Merge(plan) = change else {
            return;
        };
        let followed = async {
            match pushed_merge(plan).await? {
                Some(pushed) => daemon.follow_pushed_merge(pushed).await,
                None => Ok(()),
            }
        };
        if let Err(error) = followed.await {
            log!("CI runs of the pushed merge not followed: {error:#}");
        }
    }
}

/// What the execution of `plan` pushed: the default branch's tip, once it
/// holds the merge and the branch's upstream, as Git tracks it after the
/// push, holds the tip. A merge an earlier push failed to publish counts,
/// though this execution merged nothing new, since its push publishes it.
pub async fn pushed_merge(plan: &MergePlan) -> Result<Option<PushedMerge>> {
    let Some(push) = &plan.push else {
        return Ok(None);
    };
    let repo = &plan.repo.path;
    let default = &plan.default_refresh.branch;
    let Some(tip) = git::status::local_branch_commit(repo, default).await? else {
        return Ok(None);
    };
    let Some(tracking) = tracking_ref(repo, default).await? else {
        return Ok(None);
    };
    let holds = async |ancestor: &str, descendant: &str| {
        git::is_ancestor(repo, ancestor, descendant, git::isolated_command).await
    };
    if !holds_merge(plan, &tip).await? || !holds(tip.as_str(), &tracking).await? {
        return Ok(None);
    }
    Ok(Some(PushedMerge {
        space: plan.workspace.id.clone(),
        remotes: git::remote_push_urls(repo, &push.remote).await?,
        branch: push.branch().into(),
        commit: tip,
    }))
}

/// Whether the default branch at `tip` holds the merge of `plan`, by any
/// method: the merged branch, or its changes, which a squash or rebase
/// copied, as this execution or an earlier one merged them.
async fn holds_merge(plan: &MergePlan, tip: &CommitId) -> Result<bool> {
    git::status::merged_into(&plan.repo.path, plan.source.as_str(), tip.as_str()).await
}
