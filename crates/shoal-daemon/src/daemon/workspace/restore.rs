//! Restoring a removed space from its removal record, through one path for
//! either kind: only opening its place differs, a workspace's worktree in
//! the module below, a chat's place in another.
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::{Copied, LastSession, StoppedAgent};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::space_ids::SpaceId;
use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use super::Manager;
use crate::daemon::scope::Caller;
use crate::{
    model::{OpenedWorkspace, RemovedSpace, RestoredChat, RestoredSpace, RestoredWorkspace},
    transfer::CarriedSessionFile,
};
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Open the removed space `selector` names again, as
    /// [`Self::reopen_space`] does, unless the hub lists it on another
    /// machine.
    pub async fn restore_space(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<RestoredSpace> {
        if let Some(id) = selector.given_id() {
            self.refuse_live_elsewhere(id).await?;
        }
        self.reopen_space(selector, caller).await
    }

    /// Open the removed space `selector` names again in its place, below
    /// its recorded parent while that takes it, with its links, which
    /// `caller` relinks only where it may link, and what its record keeps of
    /// its agents; the caller links its PRs, puts the session files in place
    /// and sets a workspace up. The record goes once the space is back.
    pub(crate) async fn reopen_space(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<RestoredSpace> {
        let removed = self.claim_removed_space(selector).await?;
        let mut warnings = Vec::new();
        let parent = (self.restored_parent(&removed, caller, &mut warnings)).await?;
        let place = (self.reopen_place(&removed, parent, &mut warnings)).await?;
        let id = &removed.space_id;
        warnings.extend(self.relink_space(id, &removed.links, caller).await);
        let session_files = self.restore_agents(&removed, id, &mut warnings).await;
        if removed.saved_no_agent() {
            warnings.push(format!(
                "{} saved no session of its agent, whose restore starts a new one",
                removed.name
            ));
        }
        self.adopt_removal(&removed).await?;
        let restored = Restored {
            removed,
            session_files,
            warnings,
        };
        self.restored(place, restored).await
    }

    /// Open the space `removed` records again in its place, below `parent`:
    /// a workspace's worktree on its branch at the removed HEAD with its
    /// unsaved files, or a chat recorded, stopped, where it ran.
    async fn reopen_place(
        &self,
        removed: &RemovedSpace,
        parent: Option<SpaceId>,
        warnings: &mut Vec<String>,
    ) -> Result<Place> {
        match removed.workspace() {
            Some(_) => (self.reopen_worktree(removed, parent, warnings).await)
                .map(|opened| Place::Worktree(Box::new(opened))),
            None => (self.reopen_chat_place(removed, parent).await).map(|()| Place::Workdir),
        }
    }

    /// The space `restored` opened again in `place`, with what its restore
    /// kept for the caller.
    async fn restored(&self, place: Place, restored: Restored) -> Result<RestoredSpace> {
        let Restored {
            removed,
            session_files,
            warnings,
        } = restored;
        Ok(match place {
            Place::Worktree(opened) => RestoredSpace::Workspace(Box::new(RestoredWorkspace {
                opened: *opened,
                removed,
                session_files,
                warnings,
            })),
            Place::Workdir => {
                let chat = (self.chat_by_id(&removed.space_id).await?)
                    .context("the chat was removed meanwhile")?;
                RestoredSpace::Chat(Box::new(RestoredChat {
                    chat,
                    removed,
                    session_files,
                    warnings,
                }))
            }
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
        let here = self.find_space(&parent.id).await?;
        if here.is_none() && self.runs_elsewhere(&parent.id).await {
            return match self.reaches_elsewhere(caller, &parent.id).await? {
                true => self.scoped_chat_refusal(removed, caller).await,
                false => Ok(Some(format!(
                    "{name} was below {}, outside this space's scope, so restoring it is the user's",
                    parent.name
                ))),
            };
        }
        let refusal = match here {
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
        let spaces = self.present_spaces().await?;
        let mut held = HashMap::new();
        for space in spaces.iter().filter(|space| space.id != *id) {
            for session in crate::execution::recovery::held_sessions(&self.paths, &space.id) {
                held.entry(session).or_insert_with(|| space.name.clone());
            }
        }
        Ok(held)
    }

    /// The space the restore of `removed` opens it below: its recorded
    /// parent while that exists and takes it, as a start places a child,
    /// or runs on another joined machine;
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
        let elsewhere =
            self.find_space(&parent.id).await?.is_none() && self.runs_elsewhere(&parent.id).await;
        let taken = match elsewhere {
            true => (self.ensure_parent_elsewhere(&space, repository.as_ref(), &parent.id)).await,
            false => {
                let id = parent.id.clone();
                (self.store)
                    .read(move |tx| {
                        Ok(super::super::tree::ensure_parent(
                            tx,
                            repository.as_ref(),
                            &space,
                            &id,
                        ))
                    })
                    .await?
            }
        };
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
}

/// Where a restore opened a space again: a workspace's new worktree, or the
/// place a chat ran in, where it is recorded again.
enum Place {
    Worktree(Box<OpenedWorkspace>),
    Workdir,
}

/// What the restore of a removed space keeps for its caller: its record,
/// the session files kept with it and the warnings.
struct Restored {
    removed: RemovedSpace,
    session_files: Vec<CarriedSessionFile>,
    warnings: Vec<String>,
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

mod chats;
mod imports;
mod records;
mod worktree;

#[cfg(test)]
use records::RESTORE_CLAIM;
pub(in crate::daemon) use records::rehome_removed_chat;
pub(in crate::daemon) use records::rekey_converted;
pub(super) use worktree::{RestoredFiles, write_files};

#[cfg(test)]
pub(super) mod tests;
