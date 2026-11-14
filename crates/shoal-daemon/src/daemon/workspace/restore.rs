//! Restoring a removed space from its removal record: a workspace opens
//! again here, a chat in the module below.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use crate::execution::recovery::LastSessionExt;
use shoal_refs::{BranchName, CommitId};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use super::Manager;
use super::commits::NewRecord;
use crate::client::space_names::SpaceName;
use crate::client::space_names::SpaceSelector;
use crate::daemon::scope::Caller;
use crate::{
    git,
    model::{OpenedWorkspace, RemovedSpace, RestoredSpace, RestoredWorkspace},
    transfer::CarriedSessionFile,
};

impl Manager {
    /// Open a removed workspace again on its branch at the removed HEAD, with
    /// its unsaved files, checkpoints, linked issue, opened and related items,
    /// and stopped agents; the caller sets it up and links its PRs. The
    /// record goes once the files are back.
    pub async fn restore_workspace(&self, selector: &SpaceSelector) -> Result<RestoredWorkspace> {
        let removed = self.claim_removed_space(selector).await?;
        self.reopen_removed_workspace(removed, None).await
    }

    /// Open the removed space `selector` names again, as its kind reopens,
    /// for `caller`, which relinks only what it may link.
    pub async fn restore_space(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<RestoredSpace> {
        let removed = self.claim_removed_space(selector).await?;
        Ok(match removed.workspace() {
            Some(_) => RestoredSpace::Workspace(Box::new(
                self.reopen_removed_workspace(removed, caller).await?,
            )),
            None => RestoredSpace::Chat(Box::new(self.reopen_removed_chat(removed, caller).await?)),
        })
    }

    /// [`Self::restore_space`] for a scoped `caller`, which reopens it below
    /// its own space, which must take a child of the space's repository, as
    /// a scoped start opens one, so it stays in its tree.
    pub(crate) async fn restore_space_below(
        &self,
        selector: &SpaceSelector,
        caller: &Caller,
    ) -> Result<RestoredSpace> {
        let repository = self.restorable_below(selector, caller).await?;
        let restored = self.restore_space(selector, Some(caller)).await?;
        let space = match &restored {
            RestoredSpace::Workspace(restored) => restored.opened.workspace.id.clone(),
            RestoredSpace::Chat(restored) => restored.chat.id.clone(),
        };
        let parent = caller.space_id.clone();
        self.store
            .write(move |tx| {
                super::tree::ready_parent(tx, repository.as_ref(), &parent).with_context(|| {
                    format!("the restored space {space} stays at the top level")
                })?;
                super::tree::set_parent(tx, &space, Some(&parent))
            })
            .await?;
        Ok(restored)
    }

    /// The repository of the removed space `selector` names, once the space
    /// of `caller` takes a child of it, as a scoped restore of it places the
    /// space there.
    pub(crate) async fn restorable_below(
        &self,
        selector: &SpaceSelector,
        caller: &Caller,
    ) -> Result<Option<RepositoryId>> {
        let removed = (self.removed_spaces().await?.into_iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| format!("no restorable space {selector}; shoal restore lists them"))?;
        let repository = removed.repository_id;
        let parent = caller.space_id.clone();
        self.store
            .read(move |tx| {
                super::tree::ready_parent(tx, repository.as_ref(), &parent)?;
                Ok(repository)
            })
            .await
    }

    /// Open the removed workspace `removed` again for `caller`, which
    /// relinks only what it may link.
    async fn reopen_removed_workspace(
        &self,
        removed: RemovedSpace,
        caller: Option<&Caller>,
    ) -> Result<RestoredWorkspace> {
        let mut warnings = Vec::new();
        let new = NewRecord {
            id: self.restored_id(&removed).await?,
            parent: self.restored_parent(&removed, &mut warnings).await?,
        };
        let kept = removed.kept_workspace()?;
        let checkpoint = &kept.checkpoint;
        let files = (checkpoint.commit != checkpoint.head).then(|| RestoredFiles {
            head: checkpoint.head.to_string().into(),
            snapshot: checkpoint.commit.to_string().into(),
        });
        let opened = match kept.at.clone() {
            Some(at) => OpenedWorkspace {
                workspace: (self.restore_commit_workspace(&removed, new, at, files)).await?,
                reused: false,
            },
            None => self.restore_branch_workspace(&removed, new, files).await?,
        };
        let id = &opened.workspace.id;
        warnings.extend(self.relink_space(id, &removed.links, caller).await);
        let session_files = self.restore_agents(&removed, id, &mut warnings).await;
        self.adopt_removal(&removed, id).await?;
        Ok(RestoredWorkspace {
            opened,
            removed,
            session_files,
            warnings,
        })
    }

    /// Save for the agents of the space `id` that restores `removed` what
    /// its record keeps of them: their stopped agents and last sessions, and
    /// the session files an import carried, which it returns for the caller
    /// to put in place. Each that fails adds a warning to `warnings`.
    pub(super) async fn restore_agents(
        &self,
        removed: &RemovedSpace,
        id: &SpaceId,
        warnings: &mut Vec<String>,
    ) -> Vec<CarriedSessionFile> {
        let session_files = (self.hand_over_imported_sessions(removed, id))
            .await
            .unwrap_or_else(|error| {
                warnings.push(format!("cannot keep the agents' session files: {error:#}"));
                Vec::new()
            });
        let saved =
            crate::execution::recovery::save_stopped_agents(&self.paths, id, &removed.agents);
        if let Err(error) = saved {
            warnings.push(format!(
                "cannot save stopped agents for shoal restore: {error:#}"
            ));
        }
        for last in &removed.last_sessions {
            if let Err(error) = last.write(&self.paths, id) {
                warnings.push(format!(
                    "cannot save the last {} session for shoal restore: {error:#}",
                    last.agent
                ));
            }
        }
        session_files
    }

    /// The space the restore of `removed` opens it below: its recorded
    /// parent while that exists and takes it, as a start places a child;
    /// otherwise none, with a warning in `warnings` naming the parent.
    pub(super) async fn restored_parent(
        &self,
        removed: &RemovedSpace,
        warnings: &mut Vec<String>,
    ) -> Result<Option<SpaceId>> {
        let Some(parent) = removed.parent.clone() else {
            return Ok(None);
        };
        let (repository, space) = (removed.repository_id.clone(), removed.space_id.clone());
        let id = parent.id.clone();
        let taken = (self.store)
            .read(move |tx| {
                Ok(super::tree::ensure_parent(
                    tx,
                    repository.as_ref(),
                    &space,
                    &id,
                ))
            })
            .await?;
        match taken {
            Ok(()) => Ok(Some(parent.id)),
            Err(error) => {
                warnings.push(format!(
                    "{} opens at the top level, since its parent {} ({}) cannot take it: {error:#}",
                    removed.name, parent.name, parent.id
                ));
                Ok(None)
            }
        }
    }

    /// The ID the restore of `removed` opens it under: its own, unless the
    /// chat a conversion made of it has it, beside which it opens as a new
    /// space.
    async fn restored_id(&self, removed: &RemovedSpace) -> Result<Option<SpaceId>> {
        let taken = self.find_space(&removed.space_id).await?.is_some();
        Ok((!taken).then(|| removed.space_id.clone()))
    }

    /// Open the removed workspace `removed` again as `new` places it, on its
    /// branch at the removed HEAD, with `files` from its checkpoint.
    async fn restore_branch_workspace(
        &self,
        removed: &RemovedSpace,
        new: NewRecord,
        files: Option<RestoredFiles>,
    ) -> Result<OpenedWorkspace> {
        let branch = self.restore_branch(removed).await?;
        let base = self.restore_base(removed).await?;
        let new = super::NewWorkspace {
            options: crate::protocol::CreationOptions {
                // A name an older Shoal recorded may be longer than one set
                // now allows.
                name: Some(SpaceName::derive(removed.name.as_str(), "workspace")),
                skip_setup: removed.kept_workspace()?.setup_skipped,
                ..Default::default()
            },
            id: new.id,
            parent: new.parent,
            ..Default::default()
        };
        let opened = self
            .open_branch_restoring(removed.repository()?, branch.as_str(), base, new, files)
            .await?;
        // The branch check released the repository gate, so another request
        // may have opened the branch meanwhile; that workspace got no files.
        ensure!(
            !opened.reused,
            "workspace {} opened branch {branch} during the restore",
            opened.workspace.name
        );
        Ok(opened)
    }

    /// The branch to open: one at the removed HEAD, or at a later commit when
    /// nothing unsaved has to be applied, created at HEAD when it was deleted.
    async fn restore_branch(&self, removed: &RemovedSpace) -> Result<BranchName> {
        let (repo, _guard) = self.lock_repository(removed.repository()?).await?;
        let checkpoint = &removed.kept_workspace()?.checkpoint;
        // A detached HEAD reopens on a branch named after its short commit,
        // since nothing is built from the workspace's name.
        let head = checkpoint.head.as_str();
        let branch = (checkpoint.branch.clone())
            .unwrap_or_else(|| BranchName::from(checkpoint.head.abbreviated(7)));
        if let Some(workspace) = self.list_workspaces().await?.into_iter().find(|workspace| {
            workspace.repository_id == repo.id && workspace.branch.as_ref() == Some(&branch)
        }) {
            bail!("workspace {} already uses branch {branch}", workspace.name);
        }
        let local = git::local_ref(&branch);
        if !git::ref_exists(&repo.path, &local, git::isolated_command).await? {
            git::branch::create(&repo.path, &branch, head).await?;
            return Ok(branch);
        }
        let tip = git::verify_revision(
            &repo.path,
            &format!("{local}^{{commit}}"),
            git::run_isolated,
        )
        .await?;
        let tip = tip.trim();
        ensure!(
            git::is_ancestor(&repo.path, head, tip, git::isolated_command).await?,
            "branch {branch} no longer contains the removed HEAD {}; restore it under another name with git branch <name> {}",
            checkpoint.head,
            checkpoint.commit
        );
        ensure!(
            tip == checkpoint.head || checkpoint.commit == checkpoint.head,
            "branch {branch} moved past the removed HEAD; open it with shoal new and apply the unsaved files with git cherry-pick --no-commit {}",
            checkpoint.commit
        );
        Ok(branch)
    }

    /// The recorded base, so diffs and the stack link come back: its ref
    /// while it exists, otherwise its commit; `None` when neither remains.
    async fn restore_base(&self, removed: &RemovedSpace) -> Result<Option<String>> {
        let repo = self.repository(removed.repository()?).await?;
        let kept = removed.kept_workspace()?;
        if let Some(reference) = &kept.base_ref
            && git::ref_exists(&repo.path, reference, git::isolated_command).await?
        {
            return Ok(Some(reference.clone()));
        }
        let Some(commit) = &kept.base_commit else {
            return Ok(None);
        };
        let exists = git::verify_revision(
            &repo.path,
            &format!("{commit}^{{commit}}"),
            git::run_isolated,
        )
        .await
        .is_ok();
        Ok((exists.then(|| commit.clone())).map(String::from))
    }
}

/// The unsaved files of a removal's checkpoint, written into the new worktree.
pub(super) struct RestoredFiles {
    pub head: CommitId,
    pub snapshot: CommitId,
}

/// Write a removal checkpoint's files into a new worktree still at the removed
/// HEAD, before the workspace can be used.
pub(super) async fn write_files(worktree: &Path, files: &RestoredFiles) -> Result<()> {
    let head = git::revision(worktree, "HEAD", git::run).await?;
    ensure!(
        head.trim() == files.head,
        "the branch moved during the restore; apply the unsaved files with git cherry-pick --no-commit {}",
        files.snapshot
    );
    git::checkpoint::apply(worktree, files.snapshot.as_str()).await
}

mod chats;
mod imports;
mod records;

#[cfg(test)]
use records::RESTORE_CLAIM;

#[cfg(test)]
pub(super) mod tests;
