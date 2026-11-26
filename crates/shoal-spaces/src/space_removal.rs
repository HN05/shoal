//! What cleanup and removal do to a space of either kind: its agent, its
//! reservation and its record. They ask nothing else of a space's kind than
//! whether it has a worktree.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::Dropped;
use crate::daemon::Records;
use crate::forge::items::reference::ItemRef;
use crate::paths::StateFiles;
use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::threads::CloseCause;
use std::borrow::Cow;

use super::{
    gates::GuardMode,
    lifecycle::{Removal, Underway},
    stops::{StopPolicy, StopRequest},
    workspace::reservation::RemovalRules,
};
use crate::{
    fsutil::off_runtime,
    model::{Space, Workspace},
    removal::IdleRestore,
    state::WorkspaceState,
    time::UnixTime,
    {
        approvals::{ApprovalRequest, release_decided_by},
        events,
        notifications::AlertKind,
        spaces, threads, workspace_files,
    },
};

/// What a space's agent means for cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Running,
    /// Shoal saved its session, or a recovery is pending, to run it again.
    Saved,
    /// It stopped and nothing restores it, or none started.
    Stopped,
}

#[derive(Clone, Copy, Debug)]
pub struct SpaceAgent {
    pub state: AgentState,
    /// Where the space records it. A workspace's agent's stops count among
    /// its recorded activity.
    pub stopped_at: Option<UnixTime>,
}

/// A space a removal holds, from before it checks the space until it deletes
/// the record or gives up. Meanwhile nothing else changes the space's
/// lifecycle, restores its agent or gives it new children.
pub struct SpaceReservation<'a> {
    pub space: Space,
    _removal: spaces::Reservation<'a>,
    worktree: Option<WorktreeReservation>,
}

/// A worktree a removal holds through its workspace's lifecycle state, which
/// startup recovery reads.
struct WorktreeReservation {
    replaced: WorkspaceState,
    /// Fails the removal if it ends early.
    underway: Underway,
}

impl SpaceReservation<'_> {
    /// None for a space without a worktree.
    pub fn replaced(&self) -> Option<WorkspaceState> {
        self.worktree.as_ref().map(|worktree| worktree.replaced)
    }

    /// Call once the removal deleted the space's record.
    pub fn finish(self) {
        if let Some(worktree) = &self.worktree {
            worktree.underway.finish();
        }
    }
}

impl Spaces {
    /// Leaves out the restores of idle agents, which their wrappers wait to
    /// carry out, and the `cancelled` ones a removal stopped.
    pub async fn space_agent(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        cancelled: &[IdleRestore],
    ) -> Result<SpaceAgent> {
        let id = &space.id;
        let mut saved = self.saved_agent(daemon, id, cancelled).await?;
        // A saved chat in a worktree comes back there, as a saved agent does.
        if space.worktree().is_some() {
            for chat in self.chats_in(&space.id).await? {
                saved = saved || self.saved_agent(daemon, &chat.id, cancelled).await?;
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

    /// Leaves out the `cancelled` agents a removal stopped. An agent whose
    /// restore waits in the queue does not run.
    async fn agent_runs(&self, id: &SpaceId, cancelled: &[IdleRestore]) -> Result<bool> {
        let (id, records) = (id.to_owned(), self.records.clone());
        let running = (self.store)
            .run(move |db| records.running_agents(db, &id))
            .await?;
        Ok(running.iter().any(|execution| {
            !cancelled
                .iter()
                .any(|stopped| stopped.execution == *execution)
        }))
    }

    /// Leaves out the restores of idle agents, which their wrappers wait to
    /// carry out, and the `cancelled` ones a removal stopped.
    async fn saved_agent(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        cancelled: &[IdleRestore],
    ) -> Result<bool> {
        let idle = daemon.idle_restores(id).await?;
        crate::execution::recovery::pending_except(&self.paths, id, |execution| {
            idle.contains(execution)
                || cancelled
                    .iter()
                    .any(|stopped| stopped.execution == *execution)
        })
    }

    async fn agent_stopped_at(&self, id: &SpaceId) -> Result<Option<UnixTime>> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| events::agent_stopped_at(db, &id))
            .await
    }

    /// Refuses while any of them runs on.
    pub async fn stop_space_agent(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        policy: StopPolicy,
        request: &StopRequest,
    ) -> Result<()> {
        daemon.stop_executions(&space.id, policy, request).await
    }

    /// Reserves the worktree too, where the space has one.
    pub async fn reserve_space(
        &self,
        id: &SpaceId,
        removal: Removal<'_>,
    ) -> Result<SpaceReservation<'_>> {
        // Reserve first, so no agent of it registers and nothing opens below
        // it while the removal goes on.
        let reservation = self.space_removals.reserve(id)?;
        // A hook of the space rechecks the reservation once it holds the
        // space's resource guard. Taking the guard refuses the removal while
        // a hook runs.
        drop(self.gates.resource_guard(id, GuardMode::Shared)?);
        ensure!(
            !removal.refuses_holds() || !self.user_holds(id).await?,
            "the space has holds, which are the user's; ask the user to remove it"
        );
        let space = self.space_by_id(id).await?;
        let worktree = match space.to_workspace() {
            Some(workspace) => Some(self.reserve_worktree(&workspace, removal).await?),
            None => None,
        };
        // Read again: the reservations keep the space as it is from here on.
        let space = (self.find_space(id).await?).context("the space was removed meanwhile")?;
        Ok(SpaceReservation {
            space,
            _removal: reservation,
            worktree,
        })
    }

    /// A manual removal also takes a workspace whose setup never started, as
    /// when the command creating it was interrupted. It waits for that
    /// creation to end, then takes the repository's Git gate, under which
    /// setup starts, and reads the workspace again. A removal that holds the
    /// gate did both already.
    async fn reserve_worktree(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<WorktreeReservation> {
        let manual = matches!(removal, Removal::Manual { .. });
        // A repository's removal holds its Git gate.
        let holds_git = matches!(
            removal,
            Removal::Manual {
                for_repository: true,
                ..
            }
        );
        let mut workspace = Cow::Borrowed(workspace);
        let _created = if manual && !holds_git && workspace.state == WorkspaceState::Preparing {
            (self.gates)
                .wait_for_creation(workspace.repository(), &workspace.id)
                .await;
            let created = self.gates.lock_git(workspace.repository()).await;
            workspace = Cow::Owned(self.workspace_by_id(&workspace.id).await?);
            Some(created)
        } else {
            None
        };
        let rules = RemovalRules {
            allow_pending_rename: !off_runtime::try_exists(&workspace.path).await?,
            awaiting_setup: manual && workspace.state == WorkspaceState::Preparing,
        };
        let replaced = self
            .reserve_lifecycle_for_removal(&workspace.id, rules)
            .await?;
        Ok(WorktreeReservation {
            replaced,
            underway: Underway::new(self.store.clone(), workspace.id.clone()),
        })
    }

    /// `cause` goes to the space's journal. A worktree returns to the
    /// lifecycle state it had, or to failed once it can no longer be ready.
    pub async fn release_space(
        &self,
        reservation: SpaceReservation<'_>,
        unready: bool,
        cause: crate::model::EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        let id = &reservation.space.id;
        let Some(worktree) = reservation.worktree else {
            return crate::events::record_retained(&self.store, id, cause, error).await;
        };
        let state = if unready {
            WorkspaceState::Failed
        } else {
            worktree.replaced
        };
        (crate::events::retain_workspace(&self.store, id, state, cause, error)).await?;
        worktree.underway.finish();
        Ok(())
    }

    /// Gives the user what only the space kept: the approval requests it had
    /// not decided, the messages it had not read, and its attached files,
    /// which stay for the retention period. Its open threads close, telling
    /// their openers. The forge assignments and carried commits only its
    /// record kept go too.
    pub async fn delete_space_record(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        cause: crate::model::EventCause,
    ) -> Result<()> {
        let expires_at = self.removed_files_expiry(daemon, space).await;
        let (id, records) = (space.id.to_owned(), self.records.clone());
        let deleted = (self.store)
            .write(move |tx| delete_record(tx, &*records, &id, cause, expires_at))
            .await?;
        daemon
            .coordination()
            .ask_user_about(daemon, &deleted.released)
            .await;
        daemon
            .coordination()
            .announce_threads(daemon, deleted.threads)
            .await;
        if deleted.notification.is_some() {
            self.notifications.recorded();
        }
        (self.release_removed_assignees(daemon, space, deleted.assignees, &deleted.issues)).await;
        if let (Some(_), Some(repository)) = (space.worktree(), &space.repository_id) {
            self.release_carried(daemon, repository).await;
        }
        Ok(())
    }

    /// Takes back the user's logins `dropped` names, and the agent account on
    /// `issues` once no agent here works them. Each failure notifies.
    async fn release_removed_assignees(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        dropped: Vec<Dropped>,
        issues: &[ItemRef],
    ) {
        let mut warnings = daemon.release_assignees(dropped).await;
        match daemon.settle_removed_agents(space, issues).await {
            Ok(unsettled) => warnings.extend(unsettled),
            Err(error) => crate::log!(
                "agent assignees of removed {} not settled: {error:#}",
                &space.name
            ),
        }
        for warning in warnings {
            let kind = AlertKind::UnassignFailed;
            (daemon
                .coordination()
                .notify_space(daemon, &space.name, &space.id, kind, warning))
            .await;
        }
    }

    /// Runs once the record is deleted or, for a workspace, converted.
    /// Forgets its agents' recovery records and session logs, and a
    /// workspace's state.
    pub async fn forget_removed_space(&self, daemon: &dyn Daemon, space: &Space) {
        daemon.forget_reports(&space.id);
        if space.worktree().is_some() {
            self.forget_workspace(daemon, &space.id).await;
        }
        self.delete_session_logs(&space.id);
    }

    /// Discards what a chat the space converts into would keep: its places,
    /// its temporary directory and its attached files.
    pub async fn discard_removed_space(&self, daemon: &dyn Daemon, space: &Space) {
        daemon.close_space_places(&space.id, &space.name).await;
        self.delete_removed_tmp_dir(daemon, space).await;
        (self.delete_unrecorded_files(daemon, &space.clone().into())).await;
    }

    /// Logs a failure, since the removal already happened.
    async fn delete_removed_tmp_dir(&self, daemon: &dyn Daemon, space: &Space) {
        if let Err(error) = self.delete_tmp_dir(daemon, space).await {
            crate::log!(
                "temporary directory of {} not deleted: {error:#}",
                &space.name
            );
        }
    }

    /// Recovery records and session logs are run data the deleted record
    /// owned.
    fn delete_session_logs(&self, id: &shoal_ids::space_ids::SpaceId) {
        let _ = std::fs::remove_dir_all(self.paths.space_state(id));
    }
}

/// What [`delete_record`] gave the user.
struct Deleted {
    /// The last notification recorded for the space's unread messages.
    notification: Option<i64>,
    released: Vec<ApprovalRequest>,
    /// The user's logins the space stood for on its assigned issues.
    assignees: Vec<Dropped>,
    /// The assigned issues whose agent account the removal settles.
    issues: Vec<ItemRef>,
    threads: threads::ThreadsDelivered,
}

/// Runs once the spaces below are gone, in one transaction with what the
/// space recorded: the user gets its unread messages and the requests it
/// decides, such as an issue proposal that outlives it. Its attached files
/// stay until `expires_at`, the pending reviews it writes or of it go, and
/// its open threads close.
fn delete_record(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    id: &SpaceId,
    cause: crate::model::EventCause,
    expires_at: UnixTime,
) -> Result<Deleted> {
    // No child may outlive its parent.
    ensure!(
        super::tree::children(tx, id)?.is_empty(),
        "spaces were opened below it during removal; retaining it"
    );
    events::record(tx, id, events::SpaceEvent::Removed, cause, None)?;
    let notification = records.hand_alerts_to_user(tx, id)?;
    workspace_files::keep_removed(tx, id, expires_at)?;
    // In the same transaction, so no request is left with a removed decider.
    let released = release_decided_by(tx, id)?;
    let (assignees, issues) = records.remove_issue_links(tx, id)?;
    records.drop_pending_reviews(tx, id)?;
    let (_, threads) = threads::close(tx, records.coordination(), id, CloseCause::Removal)?;
    ensure!(
        spaces::records::remove(tx, id)?,
        "the space was removed meanwhile"
    );
    Ok(Deleted {
        notification,
        released,
        assignees,
        issues,
        threads,
    })
}
