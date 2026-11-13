//! The operations cleanup and removal take on a space whose storage still
//! differs by kind: its agent, reserving it for removal, and its record.
//! Cleanup and removal call only these, and ask nothing else of a space's
//! kind than whether it has a worktree.
use crate::client::{execution_ids::ExecutionId, space_ids::SpaceId};
use crate::paths::StateFiles;
use anyhow::{Context as _, Result, ensure};
use rusqlite::params;
use std::{borrow::Cow, collections::HashSet};

use super::{
    Manager, StopRequest,
    executions::StopPolicy,
    lifecycle::{Removal, Underway},
    reservation::RemovalRules,
};
use crate::{
    daemon::{events, spaces},
    fsutil::off_runtime,
    model::{PresentSpace, Workspace},
    removal::IdleRestore,
    state::{QueueKind, QueueState, WorkspaceState},
    time::UnixTime,
};

/// What a space's agent means for cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    /// It runs.
    Running,
    /// Shoal saved its session, or a recovery is pending, to run it again.
    Saved,
    /// It stopped and nothing restores it, or none started.
    Stopped,
}

/// A space's agent as cleanup reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpaceAgent {
    pub(crate) state: AgentState,
    /// When it last stopped, where the space records that; a workspace's
    /// agent's stops count among its recorded activity.
    pub(crate) stopped_at: Option<UnixTime>,
}

/// A space a removal holds, from before it checks the space until it deletes
/// the record or gives up: nothing else changes the space's lifecycle,
/// restores its agent or gives it new children meanwhile.
pub(crate) struct SpaceReservation<'a> {
    /// The space as recorded once reserved.
    pub(crate) space: PresentSpace,
    _removal: spaces::Reservation<'a>,
    worktree: Option<WorktreeReservation>,
}

/// A worktree a removal holds, through its workspace's lifecycle state,
/// which startup recovery reads.
struct WorktreeReservation {
    /// The lifecycle state the reservation replaced.
    replaced: WorkspaceState,
    /// The removal under way, which fails it if the removal ends early.
    underway: Underway,
}

impl SpaceReservation<'_> {
    /// Give up the reservation once its removal deleted the space's record.
    pub(crate) fn finish(self) {
        if let Some(worktree) = &self.worktree {
            worktree.underway.finish();
        }
    }
}

impl Manager {
    /// The agent of `space`, leaving out the restores of idle agents, which
    /// their wrappers wait to carry out, and the `cancelled` ones a removal
    /// stopped. A running agent is use, a workspace's as a chat's.
    pub(crate) async fn space_agent(
        &self,
        space: &PresentSpace,
        cancelled: &[IdleRestore],
    ) -> Result<SpaceAgent> {
        let id = space.id();
        let running = (self.agents_running_in(id).await?).iter().any(|execution| {
            !cancelled
                .iter()
                .any(|stopped| stopped.execution == *execution)
        });
        let idle = self.idle_restores(id).await?;
        let mut saved = self.saved_agent(id, &idle, cancelled)?;
        // A saved chat in its worktree comes back there, as a saved agent
        // does.
        if let Some(workspace) = space.worktree() {
            for chat in self.chats_in(&workspace.id).await? {
                let idle = self.idle_restores(&chat.id).await?;
                saved = saved || self.saved_agent(&chat.id, &idle, cancelled)?;
            }
        }
        Ok(SpaceAgent {
            state: match (running, saved) {
                (true, _) => AgentState::Running,
                (false, true) => AgentState::Saved,
                (false, false) => AgentState::Stopped,
            },
            stopped_at: self.agent_stopped_at(id).await?,
        })
    }

    /// The executions of the space `id` whose agent runs: those of an agent
    /// whose restore waits in the queue run none.
    async fn agents_running_in(&self, id: &SpaceId) -> Result<Vec<ExecutionId>> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| {
                db.prepare(
                    "SELECT id FROM executions e WHERE space_id=?1 AND agent IS NOT NULL
                        AND NOT EXISTS (SELECT 1 FROM agent_queue
                            WHERE kind=?2 AND state=?3 AND execution_id=e.id)",
                )?
                .query_map(params![id, QueueKind::Restore, QueueState::Queued], |row| {
                    row.get(0)
                })?
                .collect::<rusqlite::Result<_>>()
                .map_err(Into::into)
            })
            .await
    }

    /// Whether the space `id` has stopped work saved to restore, other than
    /// the `idle` restores, which their wrappers wait to carry out, and the
    /// `cancelled` ones a removal stopped.
    fn saved_agent(
        &self,
        id: &SpaceId,
        idle: &HashSet<ExecutionId>,
        cancelled: &[IdleRestore],
    ) -> Result<bool> {
        crate::execution::recovery::pending_except(&self.paths, id, |execution| {
            idle.contains(execution)
                || cancelled
                    .iter()
                    .any(|stopped| stopped.execution == *execution)
        })
    }

    /// When the agent of the space `id` last stopped or exited.
    async fn agent_stopped_at(&self, id: &SpaceId) -> Result<Option<UnixTime>> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| events::agent_stopped_at(db, &id))
            .await
    }

    /// Stop the agent of `space` and its commands for a removal, as `policy`
    /// and `request` say, and refuse while any of them runs on.
    pub(in crate::daemon::workspace) async fn stop_space_agent(
        &self,
        space: &PresentSpace,
        policy: StopPolicy,
        request: &StopRequest,
    ) -> Result<()> {
        self.stop_executions(space.id(), policy, request).await
    }

    /// Reserve the space `id` for `removal`, then its worktree, where it has
    /// one.
    pub(in crate::daemon::workspace) async fn reserve_space(
        &self,
        id: &SpaceId,
        removal: Removal<'_>,
    ) -> Result<SpaceReservation<'_>> {
        // Reserved first, so no agent of it registers and nothing opens
        // below it while the removal goes on.
        let reservation = self.space_removals.reserve(id)?;
        let space = (self.find_space(id).await?).with_context(|| format!("no space {id}"))?;
        let worktree = match space.worktree() {
            Some(workspace) => Some(self.reserve_worktree(workspace, removal).await?),
            None => None,
        };
        // Read again, as the reservations keep it from here on.
        let space = (self.find_space(id).await?).context("the space was removed meanwhile")?;
        Ok(SpaceReservation {
            space,
            _removal: reservation,
            worktree,
        })
    }

    /// Reserve the worktree of `workspace` for `removal` through its
    /// lifecycle state. A manual removal also takes a workspace whose setup
    /// never started, as when the command creating it was interrupted; it
    /// waits for that creation to end, then takes the repository's Git gate,
    /// under which setup starts, and reads the workspace again, which
    /// creation has since finished recording.
    async fn reserve_worktree(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<WorktreeReservation> {
        let manual = matches!(removal, Removal::Manual { .. });
        let mut workspace = Cow::Borrowed(workspace);
        let _created = if manual && workspace.state == WorkspaceState::Preparing {
            (self.gates)
                .wait_for_creation(&workspace.repository_id, &workspace.id)
                .await;
            let created = self.gates.lock_git(&workspace.repository_id).await;
            workspace = Cow::Owned(self.workspace_by_id(&workspace.id).await?);
            Some(created)
        } else {
            None
        };
        let rules = RemovalRules {
            allow_pending_rename: !off_runtime::try_exists(&workspace.path).await?,
            awaiting_setup: manual && workspace.state == WorkspaceState::Preparing,
            refuse_holds: removal.refuses_holds(),
        };
        let replaced = self
            .reserve_lifecycle_for_removal(&workspace.id, rules)
            .await?;
        Ok(WorktreeReservation {
            replaced,
            underway: Underway::new(self.store.clone(), workspace.id.clone()),
        })
    }

    /// Give up `reservation` after its removal failed with `error`, for
    /// `cause`, which the space's journal records: a worktree returns to the
    /// lifecycle state it had, or to failed once it can no longer be ready.
    pub(crate) async fn release_space(
        &self,
        reservation: SpaceReservation<'_>,
        unready: bool,
        cause: events::EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = reservation.space.id();
        let Some(worktree) = reservation.worktree else {
            return self.record_retained(id, cause, error).await;
        };
        let state = if unready {
            WorkspaceState::Failed
        } else {
            worktree.replaced
        };
        (self.retain_workspace(id, state, cause, error)).await?;
        worktree.underway.finish();
        Ok(())
    }

    /// Delete the record of `space`, which `cause` removed, and give the user
    /// what only it kept: the approval requests it had not decided, the
    /// messages it had not read and its attached files, which stay for the
    /// retention period.
    pub(crate) async fn delete_space_record(
        &self,
        space: &PresentSpace,
        cause: events::EventCause,
    ) -> Result<()> {
        match space {
            PresentSpace::Workspace(workspace) => {
                let released = self.commit_workspace_removal(&workspace.id, cause).await?;
                self.ask_user_about(&released).await;
                Ok(())
            }
            PresentSpace::Chat(chat) => self.delete_chat(chat).await,
        }
    }

    /// Forget what `space` kept outside its record once that is deleted or,
    /// for a workspace, converted: the places it opened, its agents'
    /// recovery records and session logs, and a workspace's state.
    pub(crate) async fn forget_removed_space(&self, space: &PresentSpace) {
        self.close_space_places(space.id(), space.name()).await;
        self.report_times.forget(space.id());
        if let PresentSpace::Workspace(workspace) = space {
            self.forget_workspace(&workspace.id).await;
        }
        self.delete_session_logs(space.id());
    }

    /// Delete the temporary directory and attached files of the removed
    /// `space`. A failure is logged, since the removal already happened.
    pub(crate) async fn delete_space_files(&self, space: &PresentSpace) {
        if let Err(error) = self.delete_tmp_dir(space).await {
            crate::daemon::log!(
                "temporary directory of {} not deleted: {error:#}",
                space.name()
            );
        }
        let deleted = match space {
            PresentSpace::Workspace(workspace) => self.delete_workspace_files(workspace).await,
            PresentSpace::Chat(_) => Ok(()),
        };
        if let Err(error) = deleted {
            crate::daemon::log!("files of {} not deleted: {error:#}", space.name());
        }
    }

    /// Recovery records and session logs are run data owned by the record
    /// just deleted.
    fn delete_session_logs(&self, id: &crate::client::space_ids::SpaceId) {
        let _ = std::fs::remove_dir_all(self.paths.space_state(id));
    }

    /// Delete the record of the workspace `id`, which `cause` removed, in
    /// one transaction with the commands and files it recorded, giving the
    /// requests it decides, such as an issue proposal that outlives it, back
    /// to the user; those given back.
    pub(in crate::daemon::workspace) async fn commit_workspace_removal(
        &self,
        id: &crate::client::space_ids::SpaceId,
        cause: events::EventCause,
    ) -> Result<Vec<crate::daemon::approvals::ApprovalRequest>> {
        let id = id.to_owned();
        self.store
            .write(move |tx| {
                // Children go first; none may outlive its parent.
                ensure!(
                    !crate::daemon::store::exists(
                        tx,
                        "SELECT 1 FROM space_parents WHERE parent_id=?1",
                        [&id],
                    )?,
                    "spaces were opened below it during removal; retaining it"
                );
                events::record(tx, &id, events::EventKind::Removed, cause, None)?;
                crate::daemon::workspace_files::forget_removed(tx, &id)?;
                let released = crate::daemon::approvals::release_decided_by(tx, &id)?;
                tx.execute("DELETE FROM space_records WHERE id=?1", [&id])?;
                Ok(released)
            })
            .await
    }
}
