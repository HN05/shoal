//! The operations cleanup and removal take on a space, whatever its kind:
//! its agent, reserving it for removal, and its record. Cleanup and removal
//! call only these, and ask nothing else of a space's kind than whether it
//! has a worktree.
use crate::client::{execution_ids::ExecutionId, space_ids::SpaceId};
use crate::paths::StateFiles;
use anyhow::{Context as _, Result, ensure};
use rusqlite::params;
use std::borrow::Cow;

use super::{
    Manager, StopRequest,
    executions::StopPolicy,
    lifecycle::{Removal, Underway},
    reservation::RemovalRules,
};
use crate::{
    daemon::{
        agent_messages,
        approvals::{ApprovalRequest, release_decided_by},
        events, spaces, workspace_files,
    },
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
        let mut saved = self.saved_agent(id, cancelled).await?;
        // A saved chat in its worktree comes back there, as a saved agent
        // does.
        if let Some(workspace) = space.worktree() {
            for chat in self.chats_in(&workspace.id).await? {
                saved = saved || self.saved_agent(&chat.id, cancelled).await?;
            }
        }
        Ok(SpaceAgent {
            state: match (self.agent_runs(id, cancelled).await?, saved) {
                (true, _) => AgentState::Running,
                (false, true) => AgentState::Saved,
                (false, false) => AgentState::Stopped,
            },
            stopped_at: self.agent_stopped_at(id).await?,
        })
    }

    /// Whether an agent of the space `id` runs, other than the `cancelled`
    /// ones a removal stopped: an agent whose restore waits in the queue
    /// does not.
    async fn agent_runs(&self, id: &SpaceId, cancelled: &[IdleRestore]) -> Result<bool> {
        let id = id.to_owned();
        let running: Vec<ExecutionId> = (self.store)
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
            .await?;
        Ok(running.iter().any(|execution| {
            !cancelled
                .iter()
                .any(|stopped| stopped.execution == *execution)
        }))
    }

    /// Whether the space `id` has stopped work saved to restore, other than
    /// the restores of idle agents, which their wrappers wait to carry out,
    /// and the `cancelled` ones a removal stopped.
    async fn saved_agent(&self, id: &SpaceId, cancelled: &[IdleRestore]) -> Result<bool> {
        let idle = self.idle_restores(id).await?;
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
        let expires_at = self.removed_files_expiry(space).await;
        let id = space.id().to_owned();
        let deleted = (self.store)
            .write(move |tx| delete_record(tx, &id, cause, expires_at))
            .await?;
        self.ask_user_about(&deleted.released).await;
        if let Some(id) = deleted.notification {
            self.signals.notification_recorded(id);
        }
        Ok(())
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
}

/// What [`delete_record`] gave the user.
struct Deleted {
    /// The last notification recorded for the space's unread messages.
    notification: Option<i64>,
    /// The approval requests the space had not decided.
    released: Vec<ApprovalRequest>,
}

/// Delete the record of the space `id`, which `cause` removed, once the
/// spaces below it are gone, in one transaction with what it recorded:
/// the user gets its unread messages and the requests it decides, such as
/// an issue proposal that outlives it, and the attached files that outlive
/// it stay, a chat's until `expires_at`.
fn delete_record(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    cause: events::EventCause,
    expires_at: UnixTime,
) -> Result<Deleted> {
    // Children go first; none may outlive its parent.
    ensure!(
        super::tree::children(tx, id)?.is_empty(),
        "spaces were opened below it during removal; retaining it"
    );
    events::record(tx, id, events::EventKind::Removed, cause, None)?;
    let notification = agent_messages::hand_to_user(tx, id)?;
    workspace_files::keep_removed(tx, id, expires_at)?;
    // With the record, so no request is left with a removed decider.
    let released = release_decided_by(tx, id)?;
    ensure!(
        tx.execute("DELETE FROM space_records WHERE id=?1", [id])? == 1,
        "the space was removed meanwhile"
    );
    Ok(Deleted {
        notification,
        released,
    })
}
