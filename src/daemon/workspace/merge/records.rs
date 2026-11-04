//! The records of local merges into the default branch, which `shoal
//! unmerge` undoes. The daemon updates them when a merge or unmerge
//! execution ends, from what the default branch holds then rather than what
//! the worker reports, and drops the ones that can no longer be undone.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};

use super::tracking_ref;
use crate::{
    daemon::{log, workspace::Manager},
    git::{self, unmerge::first_parent_chain},
    model::{ExecutionPlan, MergePlan, Repository, UnmergePlan},
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

/// A stored [`LocalMerge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedMerge {
    pub id: i64,
    pub merge: LocalMerge,
}

/// The change to the merge records an execution makes once it ends.
pub(crate) enum RecordChange {
    Merge(Box<MergePlan>),
    Unmerge(Box<UnmergePlan>),
}

impl RecordChange {
    /// The change `plan`'s execution makes, if it merges or unmerges.
    pub(crate) fn of(plan: &ExecutionPlan) -> Option<Self> {
        match (&plan.merge, &plan.unmerge) {
            (Some(merge), _) => Some(Self::Merge(merge.clone())),
            (None, Some(unmerge)) => Some(Self::Unmerge(unmerge.clone())),
            (None, None) => None,
        }
    }
}

impl Manager {
    /// Bring the merge records in line with what the execution behind
    /// `change` left the default branch at. The caller holds the repository
    /// Git gate. Failure only logs, so the execution's outcome stands.
    pub(crate) async fn update_merge_records(&self, change: &RecordChange) {
        let updated = match change {
            RecordChange::Merge(plan) => self.record_merge(plan).await,
            RecordChange::Unmerge(plan) => self.record_unmerge(plan).await,
        };
        if let Err(error) = updated {
            log!("merge records not updated: {error:#}");
        }
    }

    /// Record the merge `plan` made, if the default branch now holds it: a
    /// failed, rolled back or empty merge records nothing. Records the
    /// upstream now has go, since they can no longer be undone.
    pub(crate) async fn record_merge(&self, plan: &MergePlan) -> Result<()> {
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
        self.store.run(move |db| insert(db, &merge)).await?;
        let pushed = self.pushed_merges(&plan.repo, default).await?;
        self.forget_merges(pushed).await
    }

    /// The merges into `repo`'s `default` that it still holds, oldest
    /// first; records of merges it no longer holds go.
    pub(crate) async fn held_merges(
        &self,
        repo: &Repository,
        default: &str,
    ) -> Result<Vec<RecordedMerge>> {
        let tip = git::unmerge::tip_commit(&repo.path, &git::local_ref(default)).await?;
        let mut held = Vec::new();
        let mut gone = Vec::new();
        for recorded in self.recorded_merges(repo, default).await? {
            let result = &recorded.merge.result;
            match git::is_ancestor(&repo.path, result, &tip, git::isolated_command).await? {
                true => held.push(recorded),
                false => gone.push(recorded.id),
            }
        }
        self.forget_merges(gone).await?;
        Ok(held)
    }

    /// Update the records after the unmerge `plan` planned: drop the undone
    /// merge's, and rewrite the commits of later merges a replay recreated.
    /// A default branch the unmerge left as it was changes nothing.
    async fn record_unmerge(&self, plan: &UnmergePlan) -> Result<()> {
        let repo = &plan.repo.path;
        let Some(now) = git::status::local_branch_commit(repo, &plan.default_branch).await? else {
            return Ok(());
        };
        if now == plan.tip {
            return Ok(());
        }
        if plan.tip == plan.result {
            if now == plan.previous {
                self.forget_merges(vec![plan.merge]).await?;
            }
            return Ok(());
        }
        let Some(rewritten) = replayed_commits(plan, &now).await? else {
            return Ok(());
        };
        let (undone, repository_id, default) = (
            plan.merge,
            plan.repo.id.clone(),
            plan.default_branch.clone(),
        );
        self.store
            .run(move |db| replace_replayed(db, undone, &repository_id, &default, &rewritten))
            .await
    }

    /// The merges into `repo`'s `default` that its upstream holds, as the
    /// last fetch saw it.
    async fn pushed_merges(&self, repo: &Repository, default: &str) -> Result<Vec<i64>> {
        let Some(tracking) = tracking_ref(&repo.path, default).await? else {
            return Ok(Vec::new());
        };
        let mut pushed = Vec::new();
        for recorded in self.recorded_merges(repo, default).await? {
            let result = &recorded.merge.result;
            if git::is_ancestor(&repo.path, result, &tracking, git::isolated_command).await? {
                pushed.push(recorded.id);
            }
        }
        Ok(pushed)
    }

    async fn recorded_merges(
        &self,
        repo: &Repository,
        default: &str,
    ) -> Result<Vec<RecordedMerge>> {
        let (id, default) = (repo.id.clone(), default.to_owned());
        self.store.run(move |db| list(db, &id, &default)).await
    }

    async fn forget_merges(&self, ids: Vec<i64>) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.store.run(move |db| delete(db, &ids)).await
    }
}

/// Each commit a replay recreated, and the merge's result, mapped to the
/// commit that took its place now that the default branch is at `now`;
/// `None` when the replayed history does not match the history it replaced
/// commit for commit.
async fn replayed_commits(
    plan: &UnmergePlan,
    now: &str,
) -> Result<Option<HashMap<String, String>>> {
    let repo = &plan.repo.path;
    let (Some(replayed), Some(replays)) = (
        first_parent_chain(repo, &plan.tip, &plan.result).await?,
        first_parent_chain(repo, now, &plan.previous).await?,
    ) else {
        return Ok(None);
    };
    if replayed.len() != replays.len() {
        return Ok(None);
    }
    let mut rewritten: HashMap<String, String> = replayed.into_iter().zip(replays).collect();
    rewritten.insert(plan.result.clone(), plan.previous.clone());
    Ok(Some(rewritten))
}

/// Delete the record `undone` of the merge a replay took out, and point the
/// other records of `repository_id`'s `default` at the commits `rewritten`
/// maps theirs to, together.
fn replace_replayed(
    db: &mut Connection,
    undone: i64,
    repository_id: &str,
    default: &str,
    rewritten: &HashMap<String, String>,
) -> Result<()> {
    let tx = db.transaction()?;
    delete(&tx, &[undone])?;
    for recorded in list(&tx, repository_id, default)? {
        rewrite(&tx, &recorded, rewritten)?;
    }
    tx.commit()?;
    Ok(())
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

/// The recorded merges into `repository_id`'s `default` branch, oldest first.
fn list(db: &Connection, repository_id: &str, default: &str) -> Result<Vec<RecordedMerge>> {
    let mut statement = db.prepare(
        "SELECT id,repository_id,default_branch,workspace_id,workspace,branch,
            source_commit,previous_commit,result_commit
         FROM merges WHERE repository_id=?1 AND default_branch=?2 ORDER BY id",
    )?;
    let rows = statement.query_map(params![repository_id, default], |row| {
        Ok(RecordedMerge {
            id: row.get(0)?,
            merge: LocalMerge {
                repository_id: row.get(1)?,
                default_branch: row.get(2)?,
                workspace_id: row.get(3)?,
                workspace: row.get(4)?,
                branch: row.get(5)?,
                source: row.get(6)?,
                previous: row.get(7)?,
                result: row.get(8)?,
            },
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Point `recorded`'s default-branch commits at the commits `rewritten`
/// maps them to.
fn rewrite(
    db: &Connection,
    recorded: &RecordedMerge,
    rewritten: &HashMap<String, String>,
) -> Result<()> {
    let merge = &recorded.merge;
    let commit = |commit: &String| rewritten.get(commit).unwrap_or(commit).clone();
    let (previous, result) = (commit(&merge.previous), commit(&merge.result));
    if (&previous, &result) != (&merge.previous, &merge.result) {
        db.execute(
            "UPDATE merges SET previous_commit=?2,result_commit=?3 WHERE id=?1",
            params![recorded.id, previous, result],
        )?;
    }
    Ok(())
}

fn delete(db: &Connection, ids: &[i64]) -> Result<()> {
    for id in ids {
        db.execute("DELETE FROM merges WHERE id=?1", [id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
