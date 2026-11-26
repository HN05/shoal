//! The worktree of a chat that becomes a workspace. See
//! `docs/design/spaces.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, bail};

use super::super::workspace::{NewWorkspace, branches::OpenTarget, creation::WorkspaceSource};
use crate::git::existing_branch::BranchExt;
use crate::{
    config::modes::ModeName,
    conversions::Converting,
    git::{
        self,
        worktree::{BranchRemoval, FileRemoval},
    },
    model::{Repository, Workspace},
    protocol::CreationOptions,
};

impl Spaces {
    /// Uses the local or remote branch of that name, or else a new one from
    /// `base` or the default branch. The workspace replaces the chat's
    /// record before its worktree is made. A worktree that fails gives the
    /// chat back.
    pub async fn create_converted_workspace(
        &self,
        daemon: &dyn Daemon,
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
        let handovers = &self.conversion_handovers;
        (handovers
            .hold(self.create_converted(daemon, converting, repository, branch, base, options)))
        .await
    }

    /// Call once the hand-overs are held.
    async fn create_converted(
        &self,
        daemon: &dyn Daemon,
        converting: Converting,
        repository: shoal_ids::repository_ids::RepositoryId,
        branch: String,
        base: Option<String>,
        options: CreationOptions,
    ) -> Result<Workspace> {
        let (repo, git) = daemon
            .lock_repository(crate::repository::Selector::from(repository))
            .await?;
        git::check_branch_name(&branch)?;
        let source = self.converted_source(daemon, &repo, &branch, base).await?;
        // An existing branch goes under its local name, without its remote.
        // A new one takes the name every new branch is allocated, its
        // directory named after the branch asked for.
        let (name, branch) = match &source {
            WorkspaceSource::Existing { branch, .. } => {
                (branch.name.to_string(), branch.name.clone())
            }
            WorkspaceSource::New { .. } => {
                (branch.clone(), self.available_branch(&repo, &branch).await?)
            }
        };
        let chat = converting.chat.id.clone();
        let new = NewWorkspace {
            options,
            converts: Some(converting),
            ..Default::default()
        };
        let created =
            (self.create_branch_workspace(daemon, &repo, name, branch, source, new, git)).await;
        if created.is_err() {
            self.give_chat_back(daemon, &repo, &chat).await;
        }
        created
    }

    /// The worktree's settings may differ from the checkout's on its branch.
    pub(super) async fn ensure_worktree_mode(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        mode: &ModeName,
    ) -> Result<()> {
        let settings = daemon.workspace_settings_with_templates(workspace).await?;
        crate::conversions::ensure_mode(&settings, mode)
    }

    /// An existing branch must not be any workspace's.
    async fn converted_source(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        branch: &str,
        base: Option<String>,
    ) -> Result<WorkspaceSource> {
        if !self.branch_exists(repo, branch).await? {
            return Ok(WorkspaceSource::New { base, parent: None });
        }
        match self
            .open_target(daemon, repo, branch, Default::default())
            .await?
        {
            OpenTarget::Create(branch) => Ok(WorkspaceSource::Existing {
                branch,
                base,
                restores: None,
            }),
            OpenTarget::Reopen(workspace) => {
                bail!(
                    "branch {branch} is the branch of workspace {}",
                    workspace.name
                )
            }
        }
    }

    /// Finds branches as opening an existing branch does: by name, or with
    /// the remote, such as `origin/topic`. Without the remotes, as offline,
    /// only the local and fetched ones count.
    async fn branch_exists(&self, repo: &Repository, branch: &str) -> Result<bool> {
        let names = match git::existing_branch::list(&repo.path).await {
            Ok(branches) => (branches.iter())
                .flat_map(|existing| {
                    let remote = (existing.remote.as_ref())
                        .map(|remote| format!("{remote}/{}", existing.name));
                    [
                        Some(existing.name.to_string()),
                        remote,
                        Some(existing.selector()),
                    ]
                })
                .flatten()
                .collect(),
            Err(error) => {
                crate::log!(
                    "remote branches of {} not listed: {error:#}",
                    repo.path.display()
                );
                let mut names: Vec<String> = (git::branch::names_in_use(&repo.path).await?)
                    .into_iter()
                    .map(String::from)
                    .collect();
                if git::remote_branch(&repo.path, branch).await?.is_some() {
                    names.push(branch.to_owned());
                }
                names
            }
        };
        Ok(names.iter().any(|name| name == branch))
    }

    /// Removes what was made of the worktree once it is proven the
    /// workspace's. Without that proof, or when a step fails, the workspace
    /// stays, failed, for the user to remove, and the failure is logged.
    async fn give_chat_back(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        chat: &shoal_ids::space_ids::SpaceId,
    ) {
        let id = chat.clone();
        let pending = self
            .store
            .run(move |db| crate::conversions::pending(db, &id))
            .await;
        let row = match pending {
            Ok(Some(crate::conversions::Pending::Workspace { chat: row, .. })) => row,
            Ok(_) => return,
            Err(error) => {
                crate::log!("chat {chat} not given back: {error:#}");
                return;
            }
        };
        if let Ok(workspace) = self.workspace_by_id(chat).await
            && workspace.path.exists()
        {
            if let Err(error) = self.prove_own_worktree(daemon, &workspace).await {
                crate::log!("worktree of {} kept: {error:#}", workspace.name);
                return;
            }
            daemon.destroy_seed_outputs(&workspace).await;
            let removed = git::worktree::remove(
                &repo.path,
                &workspace.path,
                FileRemoval::Force,
                BranchRemoval::Keep,
            )
            .await;
            if let Err(error) = removed {
                crate::log!("worktree of {} not removed: {error:#}", workspace.name);
                return;
            }
        }
        let (id, records) = (chat.clone(), self.records.clone());
        let given = self
            .store
            .write(move |tx| crate::conversions::record::restore_chat(tx, &*records, &id, &row))
            .await;
        if let Err(error) = given {
            crate::log!("chat {chat} not given back: {error:#}");
        }
    }

    /// It must be registered with the repository and have the identity the
    /// workspace recorded.
    async fn prove_own_worktree(&self, daemon: &dyn Daemon, workspace: &Workspace) -> Result<()> {
        anyhow::ensure!(
            self.is_registered_worktree(daemon, workspace).await?,
            "{} is not a worktree of the repository",
            workspace.path.display()
        );
        let current = self.workspace_by_id(&workspace.id).await?;
        self.verify_worktree(daemon, &current).await
    }
}

#[cfg(test)]
mod tests;
