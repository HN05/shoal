//! The records of local merges into the default branch, which `shoal
//! unmerge` undoes. The daemon writes one when a merge execution ends, from
//! what the default branch holds then rather than what the worker reports.
use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{
    daemon::{log, workspace::Manager},
    git,
    model::MergePlan,
};

/// A local merge of a workspace's branch into its repository's default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalMerge {
    pub repository_id: String,
    pub default_branch: String,
    pub workspace_id: String,
    /// The workspace's name when it merged.
    pub workspace: String,
    pub branch: String,
    /// The branch's commit that merged.
    pub source: String,
    /// The default branch's commit before the merge.
    pub previous: String,
    /// The default branch's commit the merge left it at.
    pub result: String,
}

impl Manager {
    /// Record the merge `plan` made, if the default branch now holds it: a
    /// failed, rolled back or empty merge records nothing. Failure only logs,
    /// so the merge's outcome stands.
    pub(crate) async fn record_merge(&self, plan: &MergePlan) {
        if let Err(error) = self.try_record_merge(plan).await {
            log!("merge of {} not recorded: {error:#}", plan.branch);
        }
    }

    async fn try_record_merge(&self, plan: &MergePlan) -> Result<()> {
        let repo = &plan.repo.path;
        let default = &plan.default_refresh.branch;
        let previous = &plan.default_refresh.commit;
        let Some(result) = git::status::local_branch_commit(repo, default).await? else {
            return Ok(());
        };
        let merged = result != *previous
            && git::is_ancestor(repo, previous, &result, git::isolated_command).await?
            && git::is_ancestor(repo, &plan.source, &result, git::isolated_command).await?;
        if !merged {
            return Ok(());
        }
        let merge = LocalMerge {
            repository_id: plan.repo.id.clone(),
            default_branch: default.clone(),
            workspace_id: plan.workspace.id.clone(),
            workspace: plan.workspace.name.clone(),
            branch: plan.branch.clone(),
            source: plan.source.clone(),
            previous: previous.clone(),
            result,
        };
        self.store.run(move |db| insert(db, &merge)).await
    }
}

fn insert(db: &Connection, merge: &LocalMerge) -> Result<()> {
    db.execute(
        "INSERT INTO merges(repository_id,default_branch,workspace_id,workspace,branch,
            source_commit,previous_commit,result_commit,merged_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,unixepoch())",
        params![
            merge.repository_id,
            merge.default_branch,
            merge.workspace_id,
            merge.workspace,
            merge.branch,
            merge.source,
            merge.previous,
            merge.result,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
