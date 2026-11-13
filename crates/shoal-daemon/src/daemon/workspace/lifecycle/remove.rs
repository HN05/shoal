//! Removing a space, whichever removes it: reserving it, checking it again
//! at each step, stopping its agent, removing the spaces below it, then its
//! worktree where it has one, and deleting its record.
use super::super::{Manager, StopRequest, done::CompletedAncestor, space_removal::AgentState};
use super::worktree::{WorktreeRemoval, WorktreeStage};
use super::{Removal, Stage};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    client::output::chat::RemovedChat,
    daemon::{cleanup::Snapshot, events::EventCause, space_processes::WorktreeTies},
    model::{Completion, PresentSpace, SpaceRef},
    process::DirectoryUsers,
    removal::{IdleRestore, Inspection, InspectionPolicy, RemovalResult, UnsavedWork},
};
use anyhow::{Context, Result, bail, ensure};
use shoal_refs::CommitId;

/// Refuse to remove any of `children`, those the removal of the space named
/// `parent` takes first, when the user confirmed `removal` without one of
/// them.
fn ensure_confirmed(parent: &str, children: &[SpaceRef], removal: Removal<'_>) -> Result<()> {
    let Some(confirmed) = removal.confirmed_children() else {
        return Ok(());
    };
    let Some(late) = children.iter().find(|child| !confirmed.contains(&child.id)) else {
        return Ok(());
    };
    bail!(
        "{} joined {parent} after the removal was confirmed; retaining both",
        late.name
    )
}

/// What removing a space did.
pub(crate) struct SpaceRemoval {
    /// The spaces below it the removal took first, each after those below
    /// it.
    pub(crate) children: Vec<SpaceName>,
    /// What removing its worktree did; none for a space without one.
    pub(crate) worktree: Option<WorktreeRemoval>,
}

impl SpaceRemoval {
    /// The protocol's result of removing a workspace.
    pub(super) fn workspace_result(self) -> Result<RemovalResult> {
        let worktree = self.worktree.context("the space has no worktree")?;
        Ok(worktree.with_children(self.children))
    }
}

impl Manager {
    pub async fn remove_workspace(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let removal = Removal::Manual {
            unsaved: &unsaved,
            inspection,
            release_holds: true,
            taking: None,
        };
        self.remove_selected_workspace(selector, removal).await
    }

    /// Remove the workspace as [`Self::remove_workspace`] does, taking only
    /// the spaces `taking` names before it: one that appears in it or below
    /// it meanwhile retains them all.
    pub(crate) async fn remove_workspace_taking(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        taking: &[SpaceId],
    ) -> Result<RemovalResult> {
        let removal = Removal::Manual {
            unsaved: &unsaved,
            inspection,
            release_holds: true,
            taking: Some(taking),
        };
        self.remove_selected_workspace(selector, removal).await
    }

    /// Remove the workspace as [`Self::remove_workspace`] does, refusing it
    /// while it has holds, which are the user's and host apps'.
    pub async fn remove_unheld_workspace(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let removal = Removal::Manual {
            unsaved: &unsaved,
            inspection,
            release_holds: false,
            taking: None,
        };
        self.remove_selected_workspace(selector, removal).await
    }

    async fn remove_selected_workspace(
        &self,
        selector: &SpaceSelector,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let workspace = self.workspace(selector).await?;
        (self.remove_space(&workspace.id, removal).await?).workspace_result()
    }

    /// Remove the chat `selector` names, by ID or name, at the user's
    /// request, stopping its agent: first the spaces below it, of which
    /// `children`, when given, are the IDs the user saw and the only ones it
    /// takes. Returns the chat and the names of the spaces it took.
    pub async fn remove_chat(
        &self,
        selector: &SpaceSelector,
        children: Option<Vec<SpaceId>>,
    ) -> Result<RemovedChat> {
        let chat = self.chat(selector).await?;
        let unsaved = UnsavedWork::Confirmed {
            files: false,
            detached_head: None,
            files_restorable: false,
            children,
        };
        let removal = Removal::Manual {
            unsaved: &unsaved,
            inspection: InspectionPolicy::GitOnly,
            release_holds: true,
            taking: None,
        };
        let removed = self.remove_space(&chat.id, removal).await?;
        Ok(RemovedChat {
            chat,
            children: removed.children,
        })
    }

    /// Remove `space`, which idle or merged cleanup found unchanged as
    /// `snapshot`, for `cause`, once its grace period ends, which disk space
    /// and the user's request do not wait for; `Ok(false)` while it runs.
    pub(crate) async fn remove_unused(
        &self,
        space: &PresentSpace,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        // Under the PR gate link changes take, so no link appears while the
        // spaces below it go.
        let _gate = self.gates.lock_prs([space.id()]).await;
        (self.remove_after_grace(space, Removal::Automatic { snapshot, cause })).await
    }

    pub async fn remove_merged(&self, selector: &SpaceSelector, head: &CommitId) -> Result<bool> {
        let space = PresentSpace::Workspace(self.workspace(selector).await?);
        self.remove_after_grace(
            &space,
            Removal::Merged {
                head: head.as_str(),
            },
        )
        .await
    }

    /// Remove `space` for its `completion` with cleanup, which `cause`
    /// recorded, once its grace period ends; `Ok(false)` while it runs.
    pub(in crate::daemon::workspace) async fn remove_completed(
        &self,
        space: &PresentSpace,
        completion: &Completion,
        cause: EventCause,
    ) -> Result<bool> {
        let removal = Removal::Completed {
            head: completion.head.as_str(),
            integrated: completion.integrated,
            cause,
        };
        self.remove_after_grace(space, removal).await
    }

    /// Remove `space` below `ancestor`, whose completion with cleanup
    /// stands, once its grace period ends; `Ok(false)` while it runs.
    pub(in crate::daemon::workspace) async fn remove_under_completed(
        &self,
        space: &PresentSpace,
        ancestor: &CompletedAncestor,
    ) -> Result<bool> {
        // The sweep holds the PR gates of the space and the ancestor; the
        // ancestor's own spaces are held too, since a chat's workspaces
        // could leave it, and the space with them, after the rechecks found
        // it below the ancestor.
        let _held = self.completed_owners.reserve(&ancestor.id)?;
        self.remove_after_grace(space, Removal::UnderCompleted { ancestor })
            .await
    }

    /// Forget a workspace whose directory was deleted outside Shoal.
    pub async fn remove_deleted(&self, id: &SpaceId) -> Result<SpaceRemoval> {
        self.remove_space(id, Removal::Deleted).await
    }

    /// Remove `space` for `removal` unless its grace period still runs.
    async fn remove_after_grace(&self, space: &PresentSpace, removal: Removal<'_>) -> Result<bool> {
        if self.grace_running(space, removal).await? {
            return Ok(false);
        }
        self.remove_space(space.id(), removal).await.map(|_| true)
    }

    /// The first attempt that passes removal's initial checks starts the
    /// grace period, so it never runs while the space could not go. A
    /// failing check fails the attempt: removal would check again, and pass
    /// without any grace period once the work had changed in between.
    async fn grace_running(&self, space: &PresentSpace, removal: Removal<'_>) -> Result<bool> {
        if !removal.waits_for_grace() {
            return Ok(false);
        }
        self.check_space_removal(space.id(), removal, Stage::Initial, &[])
            .await?;
        if let Some(workspace) = space.worktree() {
            let checked = self.check_worktree(workspace, removal, Stage::Initial, &[]);
            self.check_access(&workspace.path, checked).await?;
        }
        let grace = self.present_space_settings(space).await?.cleanup.grace();
        let cleanup_at = self.claim_grace(space.id(), grace).await?;
        Ok(crate::time::UnixTime::now() < cleanup_at)
    }

    /// Remove the space `id` for `removal`: reserve it, so nothing restores
    /// or changes it meanwhile, carry the removal out, and on failure give
    /// the reservation up.
    pub(in crate::daemon::workspace) async fn remove_space(
        &self,
        id: &SpaceId,
        removal: Removal<'_>,
    ) -> Result<SpaceRemoval> {
        self.remove_space_checked(id, removal, |_| Ok(())).await
    }

    /// Remove the space `id` as [`Self::remove_space`] does, calling
    /// `checked` with the reserved space once it passes removal's first
    /// checks, before the removal changes anything; its failure fails the
    /// removal.
    pub(super) async fn remove_space_checked(
        &self,
        id: &SpaceId,
        removal: Removal<'_>,
        checked: impl FnOnce(&PresentSpace) -> Result<()> + Send,
    ) -> Result<SpaceRemoval> {
        let reservation = self.reserve_space(id, removal).await?;
        let space = reservation.space.clone();
        match self.remove_reserved(&space, removal, checked).await {
            Ok((children, worktree)) => {
                self.forget_removed_space(&space).await;
                // The chat a conversion makes keeps the space's temporary
                // directory and attached files.
                if !removal.converts() {
                    self.delete_space_files(&space).await;
                }
                let worktree = match worktree {
                    Some((stage, removed)) => {
                        Some(self.complete_worktree_removal(stage, removed).await)
                    }
                    None => None,
                };
                reservation.finish();
                Ok(SpaceRemoval { children, worktree })
            }
            Err(error) => {
                let unready = match space.worktree() {
                    Some(workspace) => self.unready_after(workspace, removal).await,
                    None => false,
                };
                (self.release_space(reservation, unready, removal.cause(), &error)).await?;
                Err(error)
            }
        }
    }

    /// Carry out `removal` of the reserved `space`: check it, stop its agent
    /// and commands, check it again, remove the spaces below it, then its
    /// worktree where it has one, once a last check passes, and delete its
    /// record, or for a conversion replace it with the chat's. Checked again
    /// after the reservation, so a space restored meanwhile cannot go by an
    /// unattended removal; `checked` learns that the first checks passed.
    async fn remove_reserved(
        &self,
        space: &PresentSpace,
        removal: Removal<'_>,
        checked: impl FnOnce(&PresentSpace) -> Result<()>,
    ) -> Result<(Vec<SpaceName>, Option<(WorktreeStage, WorktreeRemoval)>)> {
        let worktree = match space.worktree() {
            Some(workspace) => Some(self.begin_worktree_removal(workspace, removal).await?),
            None => None,
        };
        let id = space.id();
        let cause = (worktree.as_ref()).map_or(removal.cause(), |stage| stage.cause(removal));
        // A conversion's own event records what became of the space.
        if !removal.converts() {
            self.record_removal_start(id, cause).await?;
        }
        let initial =
            (self.check_space_and_worktree(id, removal, Stage::Initial, worktree.as_ref(), &[]))
                .await?;
        checked(space)?;
        // Stopping cancels the restores of idle agents that wrappers wait to
        // carry out, which exit on their own, and the others go with the
        // space; the removal record keeps the agents.
        let cancelled = initial.map(|check| check.idle_restores).unwrap_or_default();
        self.stop_for_removal(space, removal, worktree.as_ref())
            .await?;
        self.check_space_and_worktree(id, removal, Stage::AfterStop, worktree.as_ref(), &cancelled)
            .await?;
        let below = self.taken_with(id, removal).await?;
        let children = self.remove_children(space.name(), below, removal).await?;
        if let Some(worktree) = &worktree {
            self.run_removal_hooks(worktree).await?;
        }
        // After the hooks, so the final inspection and the checkpoint include
        // the files these processes write as they stop.
        let ties = (worktree.as_ref()).map_or(WorktreeTies::None, |stage| stage.ties(removal));
        self.stop_removed_processes(space, ties).await?;
        let last = self
            .check_space_and_worktree(id, removal, Stage::Final, worktree.as_ref(), &cancelled)
            .await?;
        let worktree = match worktree {
            Some(stage) => {
                let removed = self.delete_worktree(&stage, removal, last.as_ref()).await?;
                Some((stage, removed))
            }
            None => None,
        };
        if let Removal::Converted { chat } = removal {
            let released = self.commit_conversion(id, chat).await?;
            self.ask_user_about(&released).await;
        } else {
            self.delete_space_record(space, cause).await?;
        }
        Ok((children, worktree))
    }

    /// The spaces `removal` of the space `id` takes first: those below it,
    /// or for a conversion, which keeps them, the chats in its worktree.
    async fn taken_with(&self, id: &SpaceId, removal: Removal<'_>) -> Result<Vec<SpaceRef>> {
        match removal.converts() {
            true => self.chats_in(id).await,
            false => self.taken_first(id).await,
        }
    }

    /// Stop the agent of `space` and its commands for `removal`, with what
    /// else runs in its worktree.
    async fn stop_for_removal(
        &self,
        space: &PresentSpace,
        removal: Removal<'_>,
        worktree: Option<&WorktreeStage>,
    ) -> Result<()> {
        let request = match worktree {
            Some(worktree) => {
                self.stop_worktree_work(worktree).await;
                worktree.stop_request()
            }
            None => StopRequest::removal(),
        };
        (self.stop_space_agent(space, removal.stop_policy(), &request)).await
    }

    /// Apply every check `removal` needs at `stage` of the space `id`, then
    /// of its `worktree`, after stopping the `cancelled` restores, returning
    /// the worktree's inspection removal proceeds with.
    async fn check_space_and_worktree(
        &self,
        id: &SpaceId,
        removal: Removal<'_>,
        stage: Stage,
        worktree: Option<&WorktreeStage>,
        cancelled: &[IdleRestore],
    ) -> Result<Option<Inspection>> {
        self.check_space_removal(id, removal, stage, cancelled)
            .await?;
        match worktree {
            Some(worktree) => {
                (self.check_worktree_removal(worktree, removal, stage, cancelled)).await
            }
            None => Ok(None),
        }
    }

    /// Refuse `removal` of the space `id` as it is now at `stage`: an
    /// unattended one while something keeps it; any while processes of its
    /// stopped agent run on, or, first, while a space below it is in a
    /// stalled directory a sweep skips; automatic cleanup once it changed
    /// from its snapshot; completion cleanup once the completion changed;
    /// and an ancestor's completion while spaces remain below it, which go
    /// first under the same completion, or once that completion no longer
    /// takes it.
    async fn check_space_removal(
        &self,
        id: &SpaceId,
        removal: Removal<'_>,
        stage: Stage,
        cancelled: &[IdleRestore],
    ) -> Result<()> {
        let space = (self.find_space(id).await?).context("the space was removed meanwhile")?;
        // The reservation refuses new questions and PR proposals, so this
        // covers those made after the sweep's own check.
        if removal.unattended()
            && let Some(retention) = self.keep_reason(id).await?
        {
            bail!("{retention}; retaining the {}", space.kind());
        }
        // Automatic cleanup takes only spaces whose agent stopped, so one
        // restored since its snapshot goes on running.
        if matches!(removal, Removal::Automatic { .. })
            && self.space_agent(&space, cancelled).await?.state == AgentState::Running
        {
            bail!("its agent runs again; retaining the {}", space.kind());
        }
        if matches!(stage, Stage::Initial) {
            let below = self.taken_with(id, removal).await?;
            ensure_confirmed(space.name(), &below, removal)?;
            self.ensure_none_stalled_below(below, removal).await?;
        }
        // Only a worktree runs removal hooks.
        let when = match (stage, space.worktree()) {
            (Stage::Final, None) => "while the spaces below it went",
            _ => stage.change(),
        };
        let changed = || format!("{} changed {when}", space.kind());
        match (removal, stage) {
            // Later stages compare against the sweep's snapshot, so they also
            // cover changes made before removal started.
            (Removal::Automatic { .. }, Stage::Initial) => {}
            (Removal::Automatic { snapshot, .. }, Stage::AfterStop) => {
                let users = DirectoryUsers::default();
                let signals =
                    (self.candidate_signals(&space, snapshot.rule, cancelled, &users)).await?;
                ensure!(signals == Some(snapshot.signals), changed());
            }
            // The tree is compared only at the last stage: change times only
            // move forward, so that also covers changes made before it.
            (Removal::Automatic { snapshot, .. }, Stage::Final) => {
                let users = DirectoryUsers::default();
                let now =
                    (self.candidate_snapshot(&space, snapshot.rule, cancelled, &users)).await?;
                ensure!(now == Some(snapshot), changed());
            }
            (Removal::Completed { head, .. }, _) => ensure!(
                (self.completion(id).await?)
                    .is_some_and(|completion| completion.cleanup && completion.head == head),
                "the completion was withdrawn or changed since; retaining the {}",
                space.kind()
            ),
            (Removal::UnderCompleted { ancestor }, _) => {
                ensure!(
                    self.children(id).await?.is_empty(),
                    "spaces below it remain; retaining it until they go"
                );
                // A chat's worker may have been opened elsewhere since; the
                // reservation keeps it where it is from here on.
                let nearest = self.completed_ancestor(id).await?;
                ensure!(
                    nearest.is_some_and(|nearest| nearest.id == ancestor.id),
                    "{} withdrew its completion, changed or no longer has the {} below it; retaining it",
                    ancestor.name,
                    space.kind()
                );
            }
            (
                Removal::Manual { .. }
                | Removal::Merged { .. }
                | Removal::Deleted
                | Removal::Parent { .. }
                | Removal::Converted { .. },
                _,
            ) => {}
        }
        Ok(())
    }

    /// Refuse a removal that [skips stalled](Removal::skips_stalled) spaces
    /// while one of the `children` of the space it removes, or one below
    /// them, has a worktree in a directory whose file access stalled. The
    /// sweep's own pass of that space rechecks it.
    async fn ensure_none_stalled_below(
        &self,
        children: Vec<SpaceRef>,
        removal: Removal<'_>,
    ) -> Result<()> {
        if !removal.skips_stalled() {
            return Ok(());
        }
        let mut pending = children;
        while let Some(child) = pending.pop() {
            // A space without a worktree has no directory of its own; those
            // below it may.
            if let Some(space) = self.find_space(&child.id).await?
                && let Some(workspace) = space.worktree()
            {
                self.ensure_worktree_answers(workspace)?;
            }
            pending.extend(self.taken_first(&child.id).await?);
        }
        Ok(())
    }

    /// Remove `children`, the spaces the removal of the space named `parent`
    /// takes first, for `removal` of it, each after those below it,
    /// returning their names in that order. The first that fails stops the
    /// rest and retains the parent, so no space outlives its parent. Each
    /// reservation refuses children below its space and chats in its
    /// worktree, so none appears in one whose children were listed.
    async fn remove_children(
        &self,
        parent: &str,
        children: Vec<SpaceRef>,
        removal: Removal<'_>,
    ) -> Result<Vec<SpaceName>> {
        ensure_confirmed(parent, &children, removal)?;
        let mut removed = Vec::new();
        for child in children {
            // One gone meanwhile leaves nothing to take.
            if self.find_space(&child.id).await?.is_none() {
                continue;
            }
            let below = Box::pin(self.remove_space(&child.id, removal.of_child()))
                .await
                .map(|removed| removed.children)
                .with_context(|| {
                    format!("{} below it was retained, so {parent} is too", child.name)
                })?;
            removed.extend(below);
            if removal.announces_children() {
                let cleanup = crate::daemon::spaces::removal_notices::Cleanup::Parent(parent);
                self.announce_removal(&child, cleanup, &Ok(())).await?;
            }
            removed.push(child.name);
        }
        Ok(removed)
    }
}
