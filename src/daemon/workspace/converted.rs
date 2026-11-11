//! The worktree of a chat that becomes a workspace: on the branch it names,
//! an existing one or a new one, through the creation path every workspace
//! on a branch takes, with the chat's record turned into the workspace's.
use anyhow::{Result, bail};

use super::{Manager, NewWorkspace, branches::OpenTarget, creation::WorkspaceSource};
use crate::{
    daemon::conversions::Converting,
    git::{self, worktrunk},
    model::{Repository, Workspace},
    protocol::CreationOptions,
};

impl Manager {
    /// Create the workspace `converting` turns its chat into, on `branch`:
    /// the local or remote branch of that name, or else a new one from
    /// `base` or the default branch, leaving out what `options` say. It is
    /// recorded in place of the chat before its worktree is made; a
    /// worktree that fails gives the chat back.
    pub(crate) async fn create_converted_workspace(
        &self,
        converting: Converting,
        branch: String,
        base: Option<String>,
        options: CreationOptions,
    ) -> Result<Workspace> {
        let Some(repository) = converting.chat.repository_id.clone() else {
            bail!("a chat without a repository cannot become a workspace");
        };
        let (repo, _guard) = self.lock_repository(repository).await?;
        git::check_branch_name(Some(&repo.path), &branch).await?;
        let source = self.converted_source(&repo, &branch, base).await?;
        let chat = converting.chat.id.clone();
        let new = NewWorkspace {
            options,
            converts: Some(converting),
            ..Default::default()
        };
        let created =
            (self.create_branch_workspace(&repo, branch.clone(), branch, source, new)).await;
        if created.is_err() {
            self.give_chat_back(&repo, &chat).await;
        }
        created
    }

    /// Where the worktree of a converted chat starts: the existing branch
    /// `branch`, which no workspace may hold, or a new branch from `base`.
    async fn converted_source(
        &self,
        repo: &Repository,
        branch: &str,
        base: Option<String>,
    ) -> Result<WorkspaceSource> {
        if !git::branch::names_in_use(&repo.path)
            .await?
            .iter()
            .any(|name| name == branch)
        {
            return Ok(WorkspaceSource::New { base, parent: None });
        }
        match self.open_target(repo, branch, Default::default()).await? {
            OpenTarget::Create(branch) => Ok(WorkspaceSource::Existing {
                branch,
                base,
                files: None,
            }),
            OpenTarget::Reopen(workspace) => {
                bail!(
                    "branch {branch} is the branch of workspace {}",
                    workspace.name
                )
            }
        }
    }

    /// Give back the chat `chat` whose workspace failed to be made, removing
    /// what of its worktree was made. Failures are logged: the workspace
    /// then stays, failed, for the user to remove.
    async fn give_chat_back(&self, repo: &Repository, chat: &crate::client::space_ids::SpaceId) {
        let id = chat.clone();
        let pending = self
            .store
            .run(move |db| crate::daemon::conversions::pending(db, &id))
            .await;
        let row = match pending {
            Ok(Some(crate::daemon::conversions::Pending::Workspace { chat: row, .. })) => row,
            Ok(_) => return,
            Err(error) => {
                crate::daemon::log!("chat {chat} not given back: {error:#}");
                return;
            }
        };
        if let Ok(workspace) = self.workspace_by_id(chat).await
            && workspace.path.exists()
        {
            let removed = worktrunk::remove(
                &repo.path,
                &self.paths.worktrunk_config(),
                &workspace.path,
                worktrunk::FileRemoval::Force,
                worktrunk::BranchRemoval::Keep,
            )
            .await;
            if let Err(error) = removed {
                crate::daemon::log!("worktree of {} not removed: {error:#}", workspace.name);
                return;
            }
        }
        let id = chat.clone();
        let given = self
            .store
            .write(move |tx| crate::daemon::conversions::record::restore_chat(tx, &id, &row))
            .await;
        if let Err(error) = given {
            crate::daemon::log!("chat {chat} not given back: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests;
