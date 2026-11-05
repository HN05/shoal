//! Unmerge preparation: find a workspace's latest local merge and check that
//! undoing it changes only what it should before the unmerge worker moves
//! the default branch back or replays the history after it.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::OptionalExtension;

use super::{clean_checkout, local_commit, records::RecordedMerge, tracking_ref};
use crate::{
    daemon::workspace::Manager,
    git::{
        self,
        default_branch::DefaultBranchLookup,
        unmerge::{first_parent_chain, holds_commits_of, merge_has_own_changes, merged_parents},
    },
    model::{Repository, UnmergePlan, Workspace},
};

impl Manager {
    /// The workspace `selector` names for an unmerge, which runs in it: one
    /// cleanup removed has to be restored first.
    pub(crate) async fn unmerge_workspace(&self, selector: &str) -> Result<Workspace> {
        let error = match self.workspace(selector).await {
            Ok(workspace) => return Ok(workspace),
            Err(error) => error,
        };
        let selector = selector.to_owned();
        match self
            .store
            .run(move |db| removed_name(db, &selector))
            .await?
        {
            Some(name) => bail!(
                "workspace {name} was removed; restore it with shoal work restore {name}, then unmerge it"
            ),
            None => Err(error),
        }
    }

    /// Plan undoing the latest merge of the workspace `selector` names while
    /// the caller holds the repository Git gate. The gate must stay held
    /// until the tracked execution completes.
    pub async fn prepare_unmerge(&self, selector: &str) -> Result<UnmergePlan> {
        let workspace = self.workspace(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let default =
            git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let branch = workspace.require_branch("unmerging")?.to_owned();
        let held = self.held_merges(&repo, &default).await?;
        let (merge, later) = latest_merge_of(&held, &branch)
            .with_context(|| format!("{default} holds no local merge of {branch} to undo"))?;
        ensure_unpushed(&repo, &default, merge).await?;
        self.ensure_unmanaged_checkout(&repo, &default, "unmerge")
            .await?;
        let checkout = clean_checkout(&repo, &default).await?;
        let tip = local_commit(&repo, &default).await?;
        if let Some(checkout) = &checkout {
            ensure_ignored_files_kept(checkout, &default, &merge.merge.previous, &tip).await?;
        }
        if tip != merge.merge.result {
            ensure!(
                checkout.is_some(),
                "{default} is not checked out; undoing an older merge replays the later commits in its checkout"
            );
            ensure_replayable(&repo, &tip, merge, later).await?;
        }
        Ok(UnmergePlan {
            workspace,
            repo,
            default_branch: default,
            checkout,
            merge: merge.id,
            branch,
            source: merge.merge.source.clone(),
            previous: merge.merge.previous.clone(),
            result: merge.merge.result.clone(),
            tip,
        })
    }
}

/// Refuse when `checkout` has ignored files where the commit before the
/// merge or the replayed history has files: moving back or replaying writes
/// those paths, and Git overwrites ignored files without asking.
async fn ensure_ignored_files_kept(
    checkout: &Path,
    default: &str,
    previous: &str,
    tip: &str,
) -> Result<()> {
    let collisions = git::checkpoint::ignored_collisions(checkout, previous, tip).await?;
    ensure!(
        collisions.is_empty(),
        "undoing the merge would overwrite ignored files in {default}'s checkout: {}; move them away first",
        collisions.join(", ")
    );
    Ok(())
}

/// The name of the removed workspace `selector` names by name or ID.
fn removed_name(db: &rusqlite::Connection, selector: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT name FROM removed_workspaces WHERE name=?1 OR workspace_id=?1",
            [selector],
            |row| row.get(0),
        )
        .optional()?)
}

/// The latest of the `held` merges of `branch`, and the merges after it.
fn latest_merge_of<'a>(
    held: &'a [RecordedMerge],
    branch: &str,
) -> Option<(&'a RecordedMerge, &'a [RecordedMerge])> {
    let index = held
        .iter()
        .rposition(|recorded| recorded.merge.branch == branch)?;
    Some((&held[index], &held[index + 1..]))
}

/// Refuse to undo a merge whose result the default branch's upstream holds,
/// as the last fetch saw it: others may have its commits.
async fn ensure_unpushed(repo: &Repository, default: &str, merge: &RecordedMerge) -> Result<()> {
    let Some(tracking) = tracking_ref(&repo.path, default).await? else {
        return Ok(());
    };
    let pushed = git::is_ancestor(
        &repo.path,
        &merge.merge.result,
        &tracking,
        git::isolated_command,
    )
    .await?;
    ensure!(
        !pushed,
        "{default} was pushed with the merge of {branch}, so shoal unmerge cannot undo it; undo it with git revert -m 1 {result} and push that. Git then counts {branch}'s commits as merged, so merging it again brings in only newer commits",
        branch = merge.merge.branch,
        result = merge.merge.result,
    );
    Ok(())
}

/// Refuse to replay the history from the merge's result to `tip` when it
/// does not follow first parents, or when a merge in it brings in the
/// commits the unmerge takes out: the replay would put them back. A
/// recorded merge is named by its workspace, so the user can unmerge it
/// first.
async fn ensure_replayable(
    repo: &Repository,
    tip: &str,
    merge: &RecordedMerge,
    later: &[RecordedMerge],
) -> Result<()> {
    let path = &repo.path;
    let (source, previous, branch) = (
        &merge.merge.source,
        &merge.merge.previous,
        &merge.merge.branch,
    );
    let chain = first_parent_chain(path, tip, &merge.merge.result)
        .await?
        .with_context(|| format!("the history after {branch} merged does not follow its first parents, so it cannot be replayed"))?;
    for recorded in later {
        if holds_commits_of(path, &recorded.merge.source, source, previous).await? {
            bail!(
                "the later merge of {} contains {branch}'s commits, so replaying it would bring them back; unmerge it first with shoal unmerge {}",
                recorded.merge.branch,
                recorded.merge.workspace
            );
        }
    }
    for commit in &chain {
        let parents = merged_parents(path, commit).await?;
        if !parents.is_empty() && merge_has_own_changes(path, commit).await? {
            bail!(
                "the later merge commit {commit} has changes of its own, such as a conflict resolution, which replaying it would drop; undo it first"
            );
        }
        for parent in parents {
            if holds_commits_of(path, &parent, source, previous).await? {
                bail!(
                    "the later merge commit {commit} brings in {branch}'s commits, so replaying it would bring them back; undo it first"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
