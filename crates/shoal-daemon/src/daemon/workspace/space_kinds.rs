//! The operations cleanup and removal take on a space whose storage still
//! differs by kind: its agent, reserving it for removal, and its record.
//! Cleanup and removal call only these, and ask nothing else of a space's
//! kind than whether it has a worktree.
use crate::client::space_ids::SpaceId;
use crate::paths::StateFiles;
use anyhow::{Context as _, Result, ensure};

use super::{
    Manager, StopRequest,
    executions::StopPolicy,
    lifecycle::{Removal, Underway},
    reservation::RemovalRules,
};
use crate::{
    daemon::{chats, events},
    fsutil::off_runtime,
    model::PresentSpace,
    removal::IdleRestore,
    state::WorkspaceState,
    time::UnixTime,
};

/// What a space's agent means for cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    /// It runs. A workspace's running agent is among the commands its
    /// worktree's cleanup counts instead.
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
    held: Held<'a>,
}

enum Held<'a> {
    /// The workspace's lifecycle state, which the reservation replaced, and
    /// the removal under way, which fails it if the removal ends early.
    Workspace(WorkspaceState, Underway),
    Chat(#[allow(dead_code, reason = "held until it drops")] chats::Reservation<'a>),
}

impl SpaceReservation<'_> {
    /// Give up the reservation once its removal deleted the space's record.
    pub(crate) fn finish(self) {
        if let Held::Workspace(_, underway) = &self.held {
            underway.finish();
        }
    }
}

impl Manager {
    /// The agent of `space`, leaving out the restores of idle agents, which
    /// their wrappers wait to carry out, and the `cancelled` ones a removal
    /// stopped. A running chat's agent is use; a workspace's is among the
    /// commands its worktree's cleanup counts.
    pub(crate) async fn space_agent(
        &self,
        space: &PresentSpace,
        cancelled: &[IdleRestore],
    ) -> Result<SpaceAgent> {
        if let PresentSpace::Chat(chat) = space
            && chat.running
        {
            return Ok(SpaceAgent {
                state: AgentState::Running,
                stopped_at: None,
            });
        }
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
            state: if saved {
                AgentState::Saved
            } else {
                AgentState::Stopped
            },
            stopped_at: self.agent_stopped_at(id).await?,
        })
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

    /// Reserve the space `id` for `removal`. A manual removal also takes a
    /// workspace whose setup never started, as when the command creating it
    /// was interrupted; it waits for that creation to end, then takes the
    /// repository's Git gate, under which setup starts, and reads the
    /// workspace again, which creation has since finished recording.
    pub(in crate::daemon::workspace) async fn reserve_space(
        &self,
        id: &crate::client::space_ids::SpaceId,
        removal: Removal<'_>,
    ) -> Result<SpaceReservation<'_>> {
        let space = (self.find_space(id).await?).with_context(|| format!("no space {id}"))?;
        match space {
            PresentSpace::Workspace(mut workspace) => {
                let manual = matches!(removal, Removal::Manual { .. });
                let awaiting_setup = manual && workspace.state == WorkspaceState::Preparing;
                let _created = if awaiting_setup {
                    (self.gates)
                        .wait_for_creation(&workspace.repository_id, &workspace.id)
                        .await;
                    let created = self.gates.lock_git(&workspace.repository_id).await;
                    workspace = self.workspace_by_id(&workspace.id).await?;
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
                let underway = Underway::new(self.store.clone(), workspace.id.clone());
                Ok(SpaceReservation {
                    space: PresentSpace::Workspace(workspace),
                    held: Held::Workspace(replaced, underway),
                })
            }
            PresentSpace::Chat(chat) => {
                // Reserved first, so no agent of it registers while the
                // removal goes on.
                let reservation = self.chat_removals.reserve(&chat.id)?;
                let chat =
                    (self.chat_by_id(&chat.id).await?).context("the chat was removed meanwhile")?;
                Ok(SpaceReservation {
                    space: PresentSpace::Chat(chat),
                    held: Held::Chat(reservation),
                })
            }
        }
    }

    /// Give up `reservation` after its removal failed with `error`, for
    /// `cause`, which the space's journal records: a workspace returns to the
    /// state it had, or to failed once its worktree can no longer be ready.
    pub(crate) async fn release_space(
        &self,
        reservation: SpaceReservation<'_>,
        unready: bool,
        cause: events::EventCause,
        error: &anyhow::Error,
    ) -> Result<()> {
        match reservation.held {
            Held::Workspace(replaced, underway) => {
                let state = if unready {
                    WorkspaceState::Failed
                } else {
                    replaced
                };
                (self.retain_workspace(reservation.space.id(), state, cause, error)).await?;
                underway.finish();
                Ok(())
            }
            Held::Chat(_) => (self.record_retained(reservation.space.id(), cause, error)).await,
        }
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
        let deleted = match space {
            PresentSpace::Workspace(workspace) => {
                let tmp = self.delete_tmp_dir(&workspace.repository_id, &workspace.id);
                if let Err(error) = tmp.await {
                    crate::daemon::log!(
                        "temporary directory of {} not deleted: {error:#}",
                        workspace.name
                    );
                }
                self.delete_workspace_files(workspace).await
            }
            PresentSpace::Chat(chat) => self.delete_chat_tmp_dir(chat).await,
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
                tx.execute("DELETE FROM executions WHERE space_id=?1", [&id])?;
                crate::daemon::workspace_files::forget_removed(tx, &id)?;
                let released = crate::daemon::approvals::release_decided_by(tx, &id)?;
                tx.execute("DELETE FROM workspaces WHERE id=?1", [&id])?;
                Ok(released)
            })
            .await
    }
}
