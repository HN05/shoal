//! The worktree of a chat that becomes a workspace: on the branch it names,
//! an existing one or a new one, through the creation path every workspace
//! on a branch takes, with the chat's record turned into the workspace's.
use anyhow::{Result, bail};

use super::{Manager, NewWorkspace, branches::OpenTarget, creation::WorkspaceSource};
use crate::git::existing_branch::BranchExt;
use crate::{
    config::modes::ModeName,
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
        // Held until the worktree is checked or the chat given back, so no
        // relocation hands the chat's agent over to a workspace that goes.
        let _handovers = self.conversion_handovers.lock().await;
        let (repo, _guard) = self.lock_repository(repository).await?;
        git::check_branch_name(Some(&repo.path), &branch).await?;
        let source = self.converted_source(&repo, &branch, base).await?;
        // An existing branch goes under its local name, without its remote,
        // and a new one takes the name every new branch is allocated, its
        // directory named after the branch asked for.
        let (name, branch) = match &source {
            WorkspaceSource::Existing { branch, .. } => (branch.name.clone(), branch.name.clone()),
            WorkspaceSource::New { .. } => {
                (branch.clone(), self.available_branch(&repo, &branch).await?)
            }
        };
        let (chat, mode) = (converting.chat.id.clone(), converting.mode.clone());
        let new = NewWorkspace {
            options,
            converts: Some(converting),
            ..Default::default()
        };
        let created = async {
            let workspace =
                (self.create_branch_workspace(&repo, name, branch, source, new)).await?;
            self.ensure_worktree_mode(&workspace, &mode).await?;
            Ok(workspace)
        }
        .await;
        if created.is_err() {
            self.give_chat_back(&repo, &chat).await;
        }
        created
    }

    /// Refuse `mode` for the agent of the converted `workspace` unless the
    /// settings of its worktree, which may differ from the checkout's on its
    /// branch, define it.
    async fn ensure_worktree_mode(&self, workspace: &Workspace, mode: &ModeName) -> Result<()> {
        let settings = self.workspace_settings_with_templates(workspace).await?;
        crate::daemon::conversions::ensure_mode(&settings, mode)
    }

    /// Where the worktree of a converted chat starts: the existing branch
    /// `branch`, which no workspace may hold, or a new branch from `base`.
    async fn converted_source(
        &self,
        repo: &Repository,
        branch: &str,
        base: Option<String>,
    ) -> Result<WorkspaceSource> {
        if !self.branch_exists(repo, branch).await? {
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

    /// Whether `branch` names a local branch, or a branch a remote
    /// advertises, by its name or with its remote, such as `origin/topic`,
    /// as opening an existing branch finds them; without the remotes, as
    /// offline, only the local and fetched ones count.
    async fn branch_exists(&self, repo: &Repository, branch: &str) -> Result<bool> {
        let names = match git::existing_branch::list(&repo.path).await {
            Ok(branches) => (branches.iter())
                .flat_map(|existing| {
                    let remote = (existing.remote.as_ref())
                        .map(|remote| format!("{remote}/{}", existing.name));
                    [
                        Some(existing.name.clone()),
                        remote,
                        Some(existing.selector()),
                    ]
                })
                .flatten()
                .collect(),
            Err(error) => {
                crate::daemon::log!(
                    "remote branches of {} not listed: {error:#}",
                    repo.path.display()
                );
                let mut names = git::branch::names_in_use(&repo.path).await?;
                if git::remote_branch(&repo.path, branch).await?.is_some() {
                    names.push(branch.to_owned());
                }
                names
            }
        };
        Ok(names.iter().any(|name| name == branch))
    }

    /// Give back the chat `chat` whose workspace failed to be made, removing
    /// what of its worktree was made once it is proven the workspace's.
    /// Without that proof, or when a step fails, the workspace stays, failed,
    /// for the user to remove, and the failure is logged.
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
            if let Err(error) = self.prove_own_worktree(&workspace).await {
                crate::daemon::log!("worktree of {} kept: {error:#}", workspace.name);
                return;
            }
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

    /// Refuse a worktree at the path of `workspace` that is not proven to
    /// be its own: registered with the repository and of the identity it
    /// recorded.
    async fn prove_own_worktree(&self, workspace: &Workspace) -> Result<()> {
        anyhow::ensure!(
            self.is_registered_worktree(workspace).await?,
            "{} is not a worktree of the repository",
            workspace.path.display()
        );
        let current = self.workspace_by_id(&workspace.id).await?;
        self.verify_worktree(&current).await
    }
}

#[cfg(test)]
mod tests;
