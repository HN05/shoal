//! Agents' changes: the hunks Shoal records as the agents it runs change
//! the files of their worktrees. An edit tool's hook passes its edit,
//! whose hunks are the agent's; a command's hooks snapshot the worktree
//! before and after it, and what changed between is the agent's, or mixed
//! where an editor reported the user edited the same file meanwhile. What
//! changed between the agent's tool calls is someone else's. Each change
//! is numbered in its space's own sequence and goes to the thread the
//! agent works for.
use crate::{Coordination, daemon::Daemon};
use anyhow::Result;
use shoal_protocol::changes::{Change, ChangeAuthor, ChangeKind, ToolChange};

use crate::{git, model::Workspace, scope::Caller, state::WorkspaceState, time::UnixTime};
use records::{Made, NewChange};

mod attribution;
mod commands;
mod edit_reports;
mod edits;
mod listing;
pub mod records;

impl Coordination {
    /// Record what `change`, from a hook of the agent of `caller`'s space,
    /// says a tool call changed; the mixed changes it recorded, which the
    /// agent is asked about. A chat changes no files of its own, so
    /// nothing is recorded for one.
    pub async fn record_tool_change(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        change: ToolChange,
    ) -> Result<Vec<Change>> {
        let Some(workspace) = self.changing_workspace(daemon, caller).await? else {
            return Ok(Vec::new());
        };
        let _recording = self.gates.lock_changes(&workspace.id).await;
        let recorded = match change {
            ToolChange::Starts { call } => self.command_starts(&workspace, call).await?,
            ToolChange::Ends { call } => self.command_ends(&workspace, call).await?,
            ToolChange::Edited { path, hunks } => {
                self.record_edit(&workspace, &path, hunks).await?
            }
        };
        if recorded.iter().any(|change| change.original.blob.is_some())
            && let Some(repository) = &workspace.repository_id
        {
            (self.keep_code_versions(daemon, repository, [workspace.id.clone()])).await;
        }
        Ok((recorded.into_iter())
            .filter(|change| change.author == ChangeAuthor::Mixed)
            .collect())
    }

    /// The ready workspace of `caller`'s space, whose agent's changes
    /// Shoal records.
    async fn changing_workspace(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
    ) -> Result<Option<Workspace>> {
        let workspace = daemon.find_workspace(&caller.space_id).await?;
        Ok(workspace.filter(|workspace| {
            workspace.state == WorkspaceState::Ready && workspace.repository_id.is_some()
        }))
    }

    /// The snapshot `workspace`'s next change is diffed against, unless a
    /// prune deleted it, which no ref keeps; the next command then takes a
    /// new one.
    async fn change_base(&self, workspace: &Workspace) -> Result<Option<String>> {
        let id = workspace.id.clone();
        let Some(base) = (self.store).read(move |tx| records::base(tx, &id)).await? else {
            return Ok(None);
        };
        let kept = git::changes::exists(&workspace.path, &base).await?;
        Ok(kept.then_some(base))
    }

    /// Changes made in `workspace` now, at its HEAD.
    async fn made_in(&self, workspace: &Workspace) -> Result<Made> {
        let commit = (git::head_and_branch(&workspace.path).await)
            .ok()
            .map(|(commit, _)| commit);
        Ok(Made {
            space_id: workspace.id.clone(),
            space_name: workspace.name.clone(),
            repository_id: (workspace.repository_id.clone())
                .ok_or_else(|| anyhow::anyhow!("a workspace belongs to a repository"))?,
            commit,
            at: UnixTime::now(),
        })
    }
}

/// The changes of the files `changed`, each by `author` of its path: one
/// per hunk of a text file, one without hunks for another file, which is
/// never mixed, since nobody is asked about it.
fn new_changes(
    changed: Vec<git::changes::Changed>,
    author: impl Fn(&str) -> ChangeAuthor,
) -> Vec<NewChange> {
    let mut changes = Vec::new();
    for file in changed {
        let author = author(&file.path);
        if file.kind != ChangeKind::Text {
            changes.push(NewChange {
                author: match author {
                    ChangeAuthor::Mixed => ChangeAuthor::Agent,
                    author => author,
                },
                kind: file.kind,
                path: file.path,
                hunk: None,
                blob: None,
                size: Some(file.size),
            });
            continue;
        }
        changes.extend(file.hunks.into_iter().map(|hunk| NewChange {
            author,
            kind: ChangeKind::Text,
            path: file.path.clone(),
            hunk: Some(hunk),
            blob: file.blob.clone(),
            size: None,
        }));
    }
    changes
}

#[cfg(test)]
mod tests;
