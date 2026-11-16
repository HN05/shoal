//! Restoring a removed space from its removal record: a workspace opens
//! again here, a chat in the module below.
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::{Copied, LastSession, StoppedAgent};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use super::Manager;
use super::commits::NewRecord;
use crate::daemon::scope::Caller;
use crate::{
    git,
    model::{OpenedWorkspace, RemovedSpace, RestoredSpace, RestoredWorkspace},
    transfer::CarriedSessionFile,
};
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
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

    /// The removed spaces `caller` may restore: every one for the user, for a
    /// scoped caller those [`Self::scoped_restore_refusal`] lets it.
    pub(crate) async fn restorable_spaces(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Vec<RemovedSpace>> {
        let removed = self.removed_spaces().await?;
        let Some(caller) = caller else {
            return Ok(removed);
        };
        let mut restorable = Vec::new();
        for space in removed {
            if self.scoped_restore_refusal(&space, caller).await?.is_none() {
                restorable.push(space);
            }
        }
        Ok(restorable)
    }

    /// The removed space `selector` names, by ID.
    pub(crate) async fn removed_space(&self, selector: &SpaceSelector) -> Result<RemovedSpace> {
        (self.removed_spaces().await?.into_iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| format!("no restorable space {selector}; shoal restore lists them"))
    }

    /// Refuse the scoped `caller` the restore of the removed space
    /// `selector` names unless [`Self::scoped_restore_refusal`] lets it.
    pub(crate) async fn ensure_restorable_by(
        &self,
        selector: &SpaceSelector,
        caller: &Caller,
    ) -> Result<()> {
        let removed = self.removed_space(selector).await?;
        match self.scoped_restore_refusal(&removed, caller).await? {
            Some(refusal) => bail!("{refusal}"),
            None => Ok(()),
        }
    }

    /// Why the scoped `caller` may not restore `removed`, which its restore
    /// puts below its recorded parent: that space is gone or outside the
    /// caller's scope, or there is none, so the restore is the user's.
    async fn scoped_restore_refusal(
        &self,
        removed: &RemovedSpace,
        caller: &Caller,
    ) -> Result<Option<String>> {
        let name = &removed.name;
        let Some(parent) = &removed.parent else {
            return Ok(Some(format!(
                "{name} was at the top level, so restoring it is the user's"
            )));
        };
        let refusal = match self.find_space(&parent.id).await? {
            None => format!(
                "{name} was below {}, which is gone, so restoring it is the user's",
                parent.name
            ),
            Some(_) if !self.reaches(caller, &parent.id).await? => format!(
                "{name} was below {}, outside this space's scope, so restoring it is the user's",
                parent.name
            ),
            Some(_) => return self.scoped_chat_refusal(removed, caller).await,
        };
        Ok(Some(refusal))
    }

    /// Why the scoped `caller` may not restore the chat `removed`, as it
    /// could not start that chat: in the worktree of a workspace outside
    /// its scope, or with permissions its own space does not hold. A
    /// workspace that is gone is the restore's own refusal.
    async fn scoped_chat_refusal(
        &self,
        removed: &RemovedSpace,
        caller: &Caller,
    ) -> Result<Option<String>> {
        let Some(kept) = removed.chat() else {
            return Ok(None);
        };
        let name = &removed.name;
        if let Some(workspace) = &kept.workspace
            && self.find_space(&workspace.id).await?.is_some()
            && !self.reaches(caller, &workspace.id).await?
        {
            return Ok(Some(format!(
                "{name} ran in the worktree of {}, outside this space's scope, so restoring it is the user's",
                workspace.name
            )));
        }
        let repository = removed.repository_id.as_ref();
        let held =
            (self.ensure_caller_holds(Some(&caller.space_id), &kept.narrowing, repository)).await;
        Ok(held.err().map(|error| {
            format!("{name} was given permissions this space does not hold: {error:#}")
        }))
    }

    /// Open the removed workspace `removed` again on its branch at the
    /// removed HEAD, with its unsaved files, checkpoints, linked issue,
    /// opened and related items, and stopped agents, for `caller`, which
    /// relinks only what it may link; the caller sets it up and links its
    /// PRs. The record goes once the files are back.
    async fn reopen_removed_workspace(
        &self,
        removed: RemovedSpace,
        caller: Option<&Caller>,
    ) -> Result<RestoredWorkspace> {
        let mut warnings = Vec::new();
        let new = NewRecord {
            id: Some(removed.space_id.clone()),
            parent: (self.restored_parent(&removed, caller, &mut warnings)).await?,
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
        self.adopt_removal(&removed).await?;
        Ok(RestoredWorkspace {
            opened,
            removed,
            session_files,
            warnings,
        })
    }

    /// Save for the agents of the space `id` that restores `removed` what
    /// its record keeps of them: their stopped agents and last sessions, an
    /// agent whose session another space here holds to start from a fork of
    /// it, as a copy's does, and the session files an import carried, which
    /// it returns for the caller to put in place. Each fork and each failure
    /// adds a warning to `warnings`.
    pub(super) async fn restore_agents(
        &self,
        removed: &RemovedSpace,
        id: &SpaceId,
        warnings: &mut Vec<String>,
    ) -> Vec<CarriedSessionFile> {
        let session_files = (self.imported_sessions(removed))
            .await
            .unwrap_or_else(|error| {
                warnings.push(format!("cannot keep the agents' session files: {error:#}"));
                Vec::new()
            });
        let (agents, last_sessions) = self.forking_held_sessions(removed, id, warnings).await;
        let saved = crate::execution::recovery::save_stopped_agents(&self.paths, id, &agents);
        if let Err(error) = saved {
            warnings.push(format!(
                "cannot save stopped agents for shoal restore: {error:#}"
            ));
        }
        for last in &last_sessions {
            if let Err(error) = last.write(&self.paths, id) {
                warnings.push(format!(
                    "cannot save the last {} session for shoal restore: {error:#}",
                    last.agent
                ));
            }
        }
        session_files
    }

    /// The stopped agents and last sessions of `removed`, which the space
    /// `id` restores, each whose session another space here holds marked to
    /// start from a fork of it, with a warning in `warnings` naming that
    /// space.
    async fn forking_held_sessions(
        &self,
        removed: &RemovedSpace,
        id: &SpaceId,
        warnings: &mut Vec<String>,
    ) -> (Vec<StoppedAgent>, Vec<LastSession>) {
        let held = (self.sessions_held_elsewhere(id).await).unwrap_or_else(|error| {
            warnings.push(format!(
                "cannot read the sessions other spaces hold: {error:#}"
            ));
            HashMap::new()
        });
        let mut agents = removed.agents.clone();
        for agent in &mut agents {
            let record = &mut agent.record;
            if let Some(holder) = (record.session.as_ref()).and_then(|session| held.get(session)) {
                record.copied.get_or_insert(HELD_ELSEWHERE);
                warnings.push(forked(&record.agent, holder));
            }
        }
        let mut last_sessions = removed.last_sessions.clone();
        for last in &mut last_sessions {
            if let Some(holder) = (last.session.as_ref()).and_then(|session| held.get(session)) {
                last.copied.get_or_insert(HELD_ELSEWHERE);
                warnings.push(forked(&last.agent, holder));
            }
        }
        (agents, last_sessions)
    }

    /// The agent sessions the spaces here other than `id` hold, running or
    /// stopped, each with the name of a space that holds it.
    async fn sessions_held_elsewhere(
        &self,
        id: &SpaceId,
    ) -> Result<HashMap<AgentSessionId, SpaceName>> {
        let workspaces = self.list_workspaces().await?;
        let chats = self.chats().await?;
        let spaces = (workspaces
            .iter()
            .map(|workspace| (&workspace.id, &workspace.name)))
        .chain(chats.iter().map(|chat| (&chat.id, &chat.name)));
        let mut held = HashMap::new();
        for (space, name) in spaces.filter(|(space, _)| *space != id) {
            for session in crate::execution::recovery::held_sessions(&self.paths, space) {
                held.entry(session).or_insert_with(|| name.clone());
            }
        }
        Ok(held)
    }

    /// The space the restore of `removed` opens it below: its recorded
    /// parent while that exists and takes it, as a start places a child;
    /// otherwise none, with a warning in `warnings` naming the parent, or
    /// for a scoped `caller`, which restores only below it, a refusal.
    pub(super) async fn restored_parent(
        &self,
        removed: &RemovedSpace,
        caller: Option<&Caller>,
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
            Err(error) if caller.is_some() => Err(error.context(format!(
                "{} goes below {} only, which cannot take it",
                removed.name, parent.name
            ))),
            Err(error) => {
                warnings.push(format!(
                    "{} opens at the top level, since its parent {} ({}) cannot take it: {error:#}",
                    removed.name, parent.name, parent.id
                ));
                Ok(None)
            }
        }
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
            workspace.repository() == &repo.id && workspace.branch() == Some(&branch)
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

/// How a restored agent whose session another space holds starts: from a
/// fork of it, as a copy's does.
const HELD_ELSEWHERE: Copied = Copied {
    earlier_files: false,
    held_elsewhere: true,
};

/// The warning that `agent` continues from a fork of its session, which
/// the space `holder` holds too.
fn forked(agent: &crate::agent::Agent, holder: &SpaceName) -> String {
    format!("the {agent} session continues from a fork, since {holder} holds it too")
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
pub(super) use records::{rehome_removed_chat, rekey_converted};

#[cfg(test)]
pub(super) mod tests;
