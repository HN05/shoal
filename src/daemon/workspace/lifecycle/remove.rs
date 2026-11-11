//! Removing a workspace: reserving it, the steps that remove its worktree
//! and the spaces below it, and committing the removal.
use super::super::{Manager, StopRequest, done::CompletedAncestor, reservation::RemovalRules};
use super::{Removal, Stage};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::daemon::space_processes::WorktreeTies;
use crate::{
    daemon::{
        cleanup::Snapshot,
        events::{self, EventCause, EventKind},
    },
    fsutil::off_runtime,
    git::worktrunk::{self, BranchRemoval, FileRemoval},
    hooks::{self, Hook, HookKind},
    model::{RemovedWorkspace, SpaceKind, SpaceRef, Workspace},
    removal::{
        self, BranchOutcome, IdleRestore, Inspection, InspectionPolicy, RemovalCheck,
        RemovalResult, UnsavedWork,
    },
    state::WorkspaceState,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

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
    let started = match late.kind {
        SpaceKind::Workspace => "opened below",
        SpaceKind::Chat => "started in",
    };
    bail!(
        "{} was {started} {parent} after the removal was confirmed; retaining both",
        late.name
    )
}

/// A workspace reserved for removal: its record, whether its directory
/// existed, and the state the reservation replaced.
pub(super) struct Reserved {
    pub(super) workspace: Workspace,
    present: bool,
    replaced: WorkspaceState,
}

/// A post-remove hook resolved while the workspace record still exists.
struct PostRemoveHook {
    command: PathBuf,
    checkout: PathBuf,
    env: BTreeMap<String, OsString>,
}

impl Manager {
    /// What removing the workspace at the user's request would do, with the
    /// spaces it takes first, deepest first, and its changed files and whether
    /// removal archives them when `include_changes` is set and it has any.
    pub async fn check_manual_removal(
        &self,
        selector: &SpaceSelector,
        include_changes: bool,
    ) -> Result<RemovalCheck> {
        let mut check = self
            .check_removal(selector, InspectionPolicy::GitOnly)
            .await?
            .check;
        check.children = self.spaces_taken_with(&check.workspace.id).await?;
        check.children.reverse();
        for child in &check.children {
            if child.kind == crate::model::SpaceKind::Workspace
                && self.workspace_by_id(&child.id).await?.disposable
            {
                check.disposable_children.push(child.id.clone());
            }
        }
        if include_changes && check.dirty {
            removal::load_changed_files(&mut check).await?;
            check.unsaved_files_archived = self.archives_unsaved_files(&check.workspace).await?;
        }
        Ok(check)
    }

    pub async fn check_removal(
        &self,
        selector: &SpaceSelector,
        policy: InspectionPolicy,
    ) -> Result<Inspection> {
        let workspace = self.workspace(selector).await?;
        let executions = self.cleanup_executions(&workspace.id, &[]).await?;
        self.inspect_removal(workspace, policy, executions).await
    }

    pub async fn remove_workspace(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let unsaved = &unsaved;
        self.remove(
            selector,
            Removal::Manual {
                unsaved,
                inspection,
                release_holds: true,
                taking: None,
            },
        )
        .await
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
        let unsaved = &unsaved;
        self.remove(
            selector,
            Removal::Manual {
                unsaved,
                inspection,
                release_holds: true,
                taking: Some(taking),
            },
        )
        .await
    }

    /// Remove the workspace as [`Self::remove_workspace`] does, refusing it
    /// while it has holds, which are the user's and host apps'.
    pub async fn remove_unheld_workspace(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let unsaved = &unsaved;
        self.remove(
            selector,
            Removal::Manual {
                unsaved,
                inspection,
                release_holds: false,
                taking: None,
            },
        )
        .await
    }

    /// `Ok(false)` while the grace period runs, as for every unattended removal.
    pub async fn remove_idle(&self, selector: &SpaceSelector, snapshot: Snapshot) -> Result<bool> {
        self.remove_unused(selector, snapshot, EventCause::Idle)
            .await
    }

    pub async fn remove_merged_idle(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<bool> {
        self.remove_unused(selector, snapshot, EventCause::Merged)
            .await
    }

    /// Remove an idle cleanup candidate without waiting for its idle delay.
    pub async fn remove_for_disk_space(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::DiskSpace)
            .await
            .map(|_| ())
    }

    /// Remove an idle cleanup candidate on the user's request, without
    /// waiting for its idle delay.
    pub async fn remove_on_request(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
    ) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::Manual)
            .await
            .map(|_| ())
    }

    async fn remove_unused(
        &self,
        selector: &SpaceSelector,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        let (workspace, _guard) = self.lock_pr_gate(selector).await?;
        self.remove_after_grace(
            &SpaceSelector::id(&workspace.id),
            Removal::Automatic { snapshot, cause },
        )
        .await
    }

    pub async fn remove_merged(&self, selector: &SpaceSelector, head: &str) -> Result<bool> {
        self.remove_after_grace(selector, Removal::Merged { head })
            .await
    }

    pub(in crate::daemon::workspace) async fn remove_completed(
        &self,
        selector: &SpaceSelector,
        completion: &crate::model::Completion,
        cause: EventCause,
    ) -> Result<bool> {
        let removal = Removal::Completed {
            head: &completion.head,
            integrated: completion.integrated,
            cause,
        };
        self.remove_after_grace(selector, removal).await
    }

    /// Remove the workspace below `ancestor`, whose completion with cleanup
    /// stands, after the grace period.
    pub(in crate::daemon::workspace) async fn remove_under_completed(
        &self,
        selector: &SpaceSelector,
        ancestor: &CompletedAncestor,
    ) -> Result<bool> {
        // A chat's workspaces could leave it, and the workspace with them,
        // after the rechecks found it below the chat.
        let _held = match ancestor {
            CompletedAncestor::Chat(chat) => Some(self.chat_owners.reserve(&chat.id)?),
            CompletedAncestor::Workspace(_) => None,
        };
        self.remove_after_grace(selector, Removal::UnderCompleted { ancestor })
            .await
    }

    /// Remove the workspace unless its grace period is still running.
    async fn remove_after_grace(
        &self,
        selector: &SpaceSelector,
        removal: Removal<'_>,
    ) -> Result<bool> {
        let workspace = self.workspace(selector).await?;
        if self.grace_running(&workspace, removal).await? {
            return Ok(false);
        }
        self.remove(selector, removal).await.map(|_| true)
    }

    /// The first attempt that passes removal's initial checks starts the
    /// grace period, so it never runs while the workspace could not go. A
    /// failing check fails the attempt: removal would check again, and pass
    /// without any grace period once the work had changed in between.
    async fn grace_running(&self, workspace: &Workspace, removal: Removal<'_>) -> Result<bool> {
        if !removal.waits_for_grace() {
            return Ok(false);
        }
        let checked = self.recheck_removal(workspace, removal, Stage::Initial, &[]);
        self.check_access(&workspace.path, checked).await?;
        let grace = self.workspace_settings(workspace).await?.cleanup.grace();
        let cleanup_at = self.claim_grace(&workspace.id, grace).await?;
        Ok(crate::time::UnixTime::now() < cleanup_at)
    }

    /// Forget a workspace whose directory was deleted outside Shoal.
    pub async fn remove_deleted(&self, selector: &SpaceSelector) -> Result<RemovalResult> {
        self.remove(selector, Removal::Deleted).await
    }

    pub(super) async fn remove(
        &self,
        selector: &SpaceSelector,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let reserved = self.reserve_removal(selector, removal).await?;
        self.remove_reserved(reserved, removal).await
    }

    /// Refuse now what removing the `reserved` workspace would refuse
    /// before it changes anything, retaining the workspace, so a removal that
    /// goes on in the background fails only on what changes meanwhile.
    pub(super) async fn check_reserved_removal(
        &self,
        reserved: &Reserved,
        removal: Removal<'_>,
    ) -> Result<()> {
        let workspace = &reserved.workspace;
        let checked = async {
            ensure_confirmed(
                &workspace.name,
                &self.taken_first(&workspace.id).await?,
                removal,
            )?;
            if !reserved.present {
                ensure!(
                    self.missing_worktree(workspace).await?.is_none(),
                    "worktree was moved; restore its recorded path before removing it"
                );
                return Ok(());
            }
            self.ensure_no_pending_rename(&workspace.id).await?;
            self.recheck_removal(workspace, removal, Stage::Initial, &[])
                .await
                .map(drop)
        }
        .await;
        if let Err(error) = &checked {
            self.retain_workspace(&workspace.id, reserved.replaced, removal.cause(), error)
                .await?;
        }
        checked
    }

    /// Remove the workspace `reserved` holds for `removal`, retaining it
    /// when a step fails.
    pub(super) async fn remove_reserved(
        &self,
        reserved: Reserved,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let Reserved {
            workspace,
            present,
            replaced: reserved_state,
        } = reserved;
        // Checked again after the reservation, so a worktree restored meanwhile
        // cannot be deleted by an unattended removal.
        let result = async {
            // Its reviews go with its record.
            self.stop_workspace_review(&workspace.id).await;
            // A CI job ends through its forge, before its runner is stopped.
            self.cancel_job_before_stopping(&workspace.id).await;
            let present = off_runtime::try_exists(&workspace.path).await?;
            let post_remove = if present {
                self.ensure_no_pending_rename(&workspace.id).await?;
                self.prepare_post_remove_hook(&workspace).await?
            } else {
                None
            };
            let xcode = self.xcode_settings(&workspace, present).await;
            let outcome = match removal {
                Removal::Deleted if present => bail!("workspace directory exists"),
                _ if present => self.remove_present_worktree(&workspace, removal).await?,
                _ => self.remove_missing_worktree(&workspace, removal).await?,
            };
            // While the record still reserves the path: a workspace created
            // there gets the same DerivedData.
            if let Some(settings) = xcode {
                self.delete_xcode_data(&workspace, &settings).await;
            }
            let released = self
                .commit_workspace_removal(
                    &workspace.id,
                    if present {
                        removal.cause()
                    } else {
                        EventCause::MissingDirectory
                    },
                )
                .await?;
            Ok((outcome, post_remove, released))
        }
        .await;
        match result {
            Ok((outcome, post_remove, released)) => {
                self.ask_user_about(&released).await;
                Ok(self
                    .complete_workspace_removal(&workspace, outcome, post_remove)
                    .await)
            }
            Err(error) => {
                // A deleted directory can no longer be ready.
                let state = match removal {
                    Removal::Deleted if !present => WorkspaceState::Failed,
                    _ => reserved_state,
                };
                self.retain_workspace(&workspace.id, state, removal.cause(), &error)
                    .await?;
                Err(error)
            }
        }
    }

    /// Reserve the workspace `selector` names for `removal`. A manual removal
    /// also takes one whose setup never started, as when the command creating
    /// it was interrupted; it waits on the repository's Git gate for that
    /// creation to end and reads the workspace again, which creation has
    /// since finished recording.
    pub(super) async fn reserve_removal(
        &self,
        selector: &SpaceSelector,
        removal: Removal<'_>,
    ) -> Result<Reserved> {
        let manual = matches!(removal, Removal::Manual { .. });
        let mut workspace = self.workspace(selector).await?;
        let _created = if manual && workspace.state == WorkspaceState::Preparing {
            let created = self.gates.lock_git(&workspace.repository_id).await;
            workspace = self.workspace_by_id(&workspace.id).await?;
            Some(created)
        } else {
            None
        };
        let present = off_runtime::try_exists(&workspace.path).await?;
        let rules = RemovalRules {
            allow_pending_rename: !present,
            awaiting_setup: manual && workspace.state == WorkspaceState::Preparing,
            refuse_holds: removal.refuses_holds(),
        };
        let replaced = self
            .reserve_lifecycle_for_removal(&workspace.id, rules)
            .await?;
        Ok(Reserved {
            workspace,
            present,
            replaced,
        })
    }

    /// The removal hook `kind` of `workspace`; none for one whose setup was
    /// skipped, since they tear down what setup made.
    async fn removal_hook(
        &self,
        workspace: &Workspace,
        kind: HookKind,
    ) -> Result<Option<std::path::PathBuf>> {
        if workspace.setup_skipped {
            return Ok(None);
        }
        self.workspace_hook(workspace, kind).await
    }

    async fn prepare_post_remove_hook(
        &self,
        workspace: &Workspace,
    ) -> Result<Option<PostRemoveHook>> {
        match self.removal_hook(workspace, HookKind::PostRemove).await? {
            Some(command) => Ok(Some(PostRemoveHook {
                command,
                checkout: self.repository(&workspace.repository_id).await?.path,
                env: self.removed_workspace_environment(workspace).await?,
            })),
            None => Ok(None),
        }
    }

    /// Release ownership and cascading resource leases in one transaction,
    /// giving the requests the workspace decides, such as an issue proposal
    /// that outlives its workspace, back to the user; those given back.
    pub(super) async fn commit_workspace_removal(
        &self,
        id: &SpaceId,
        cause: EventCause,
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
                events::record(tx, &id, EventKind::Removed, cause, None)?;
                tx.execute("DELETE FROM executions WHERE workspace_id=?1", [&id])?;
                crate::daemon::workspace_files::forget_removed(tx, &id)?;
                let released = crate::daemon::approvals::release_decided_by(tx, &id)?;
                tx.execute("DELETE FROM workspaces WHERE id=?1", [&id])?;
                Ok(released)
            })
            .await
    }

    /// Best-effort cleanup after ownership is released cannot fail removal.
    async fn complete_workspace_removal(
        &self,
        workspace: &Workspace,
        mut outcome: RemovalResult,
        post_remove: Option<PostRemoveHook>,
    ) -> RemovalResult {
        self.close_space_places(&workspace.id, &workspace.name)
            .await;
        self.forget_workspace(&workspace.id).await;
        if let Err(error) = self.remember_kept_branch(workspace, &outcome).await {
            crate::daemon::log!(
                "kept branch of {} not remembered: {error:#}",
                workspace.name
            );
        }
        self.delete_session_logs(&workspace.id);
        if let Err(error) = self
            .delete_tmp_dir(&workspace.repository_id, &workspace.id)
            .await
        {
            crate::daemon::log!(
                "temporary directory of {} not deleted: {error:#}",
                workspace.name
            );
        }
        if let Err(error) = self.delete_workspace_files(workspace).await {
            crate::daemon::log!(
                "files attached to messages of {} not deleted: {error:#}",
                workspace.name
            );
        }
        if let Some(hook) = post_remove {
            outcome.hook_error = self.run_post_remove_hook(workspace, &hook).await;
            // The hook runs with the workspace identity after removal's sweep.
            if let Err(error) = self
                .stop_removed_processes(workspace, WorktreeTies::None)
                .await
            {
                crate::daemon::log!(
                    "processes left by the post-remove hook of {} not stopped: {error:#}",
                    workspace.name
                );
            }
        }
        outcome
    }

    /// Session logs are run data owned by the record just deleted.
    fn delete_session_logs(&self, id: &SpaceId) {
        let _ = std::fs::remove_dir_all(self.paths.workspace_state(id));
    }

    /// Run the post-remove hook, notifying and returning its failure.
    async fn run_post_remove_hook(
        &self,
        workspace: &Workspace,
        hook: &PostRemoveHook,
    ) -> Option<String> {
        let error = hooks::run_detached(
            Hook::PostRemove(&hook.checkout),
            workspace,
            &hook.command,
            &self.paths,
            &hook.env,
        )
        .await
        .err()?;
        let message = format!("workspace removed; {error:#}");
        self.notify(
            Some(&workspace.name),
            crate::daemon::notifications::NotificationKind::HookFailed,
            &message,
        )
        .await;
        Some(message)
    }

    /// Remove an existing worktree after a checkpoint keeps its HEAD and
    /// unsaved files.
    async fn remove_present_worktree(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let repo = self.repository(&workspace.repository_id).await?;
        let initial = self
            .recheck_removal(workspace, removal, Stage::Initial, &[])
            .await?;
        // Stopping cancels the restores of idle agents that wrappers wait to
        // carry out, which exit on their own, and the others go with the
        // workspace; the removal record keeps the agents.
        let cancelled = initial.idle_restores;
        self.stop_executions(
            &workspace.id,
            removal.stop_policy(),
            &StopRequest::restorable_removal(),
        )
        .await?;
        self.recheck_removal(workspace, removal, Stage::AfterStop, &cancelled)
            .await?;
        let children = self.remove_workspace_children(workspace, removal).await?;
        self.run_pre_remove_hook(workspace).await?;
        self.run_resource_release_hooks(workspace).await?;
        // Processes can write files as they stop, which the final inspection
        // and the checkpoint must include.
        self.stop_removed_processes(workspace, removal.worktree_ties())
            .await?;
        let check = self
            .recheck_removal(workspace, removal, Stage::AfterHooks, &cancelled)
            .await?;
        let removed = self
            .delete_checkpointed_worktree(&repo.path, workspace, removal, &check)
            .await?;
        Ok(RemovalResult {
            children,
            ..removed
        })
    }

    /// Run the pre-remove hook, which sees the worktree intact; a failing
    /// hook retains the workspace.
    async fn run_pre_remove_hook(&self, workspace: &Workspace) -> Result<()> {
        let Some(command) = self.removal_hook(workspace, HookKind::PreRemove).await? else {
            return Ok(());
        };
        let configured_env = self.configured_workspace_environment(workspace).await?;
        hooks::run_detached(
            Hook::PreRemove,
            workspace,
            &command,
            &self.paths,
            &configured_env,
        )
        .await
    }

    /// Run the pre-resource-release hook for each resource lease of the
    /// workspace, which committing the removal releases. The hook is resolved
    /// only for leases to release, since it fails while the worktree's config
    /// file is rejected.
    async fn run_resource_release_hooks(&self, workspace: &Workspace) -> Result<()> {
        let leases = self
            .list_resources(Some(&SpaceSelector::id(&workspace.id)))
            .await?;
        if leases.is_empty() {
            return Ok(());
        }
        let release_hook = self
            .workspace_hook(workspace, HookKind::PreResourceRelease)
            .await?;
        for lease in leases {
            self.run_resource_release_hook(workspace, &lease, release_hook.as_deref())
                .await?;
        }
        Ok(())
    }

    /// Delete the worktree as the final inspection `check` decides, once a
    /// checkpoint keeps its HEAD and the unsaved files `removal` discards or
    /// promises to keep. A failed deletion may have deleted some files, so
    /// its checkpoint stays; the next attempt takes another, and both expire
    /// as usual.
    async fn delete_checkpointed_worktree(
        &self,
        repo: &Path,
        workspace: &Workspace,
        removal: Removal<'_>,
        check: &Inspection,
    ) -> Result<RemovalResult> {
        let record = self.save_restore_record(workspace, removal).await?;
        ensure_checkpoint_keeps_files(check, removal, record.as_ref())?;
        let (files, branch) = worktrunk_removal_options(check, removal);
        self.remove_worktree(repo, workspace, files, branch, true)
            .await
    }

    /// The record that restores the workspace, with a checkpoint of its HEAD
    /// and unsaved files; a disposable workspace keeps nothing to restore.
    async fn save_restore_record(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<Option<RemovedWorkspace>> {
        if workspace.disposable {
            return Ok(None);
        }
        self.save_removal_record(workspace, removal.cause())
            .await
            .map(Some)
    }

    /// Inspect the workspace again and apply every check `removal` needs at
    /// `stage`, after stopping the `cancelled` restores, returning the
    /// inspection removal proceeds with.
    async fn recheck_removal(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
        stage: Stage,
        cancelled: &[IdleRestore],
    ) -> Result<Inspection> {
        let executions = self.cleanup_executions(&workspace.id, cancelled).await?;
        let check = self
            .inspect_removal(
                self.workspace_by_id(&workspace.id).await?,
                removal.inspection(),
                executions,
            )
            .await?;
        // The reservation refuses new questions and PR proposals, so this
        // covers those made after the sweep's own check.
        if removal.unattended()
            && let Some(retention) = self.keep_reason(&workspace.id).await?
        {
            bail!("{retention}; retaining workspace");
        }
        if matches!(stage, Stage::Initial) {
            let below = self.taken_first(&workspace.id).await?;
            self.ensure_none_stalled_below(below, removal).await?;
        }
        removal.verify(&check, stage)?;
        match (removal, stage) {
            (Removal::Merged { head } | Removal::Completed { head, .. }, _) => ensure!(
                crate::daemon::links::pr::current_head(workspace).await? == head,
                "HEAD changed {}",
                stage.change()
            ),
            // Later stages compare against the sweep's snapshot, so they also
            // cover changes made before removal started.
            (Removal::Automatic { .. }, Stage::Initial) => {}
            (Removal::Automatic { snapshot, .. }, Stage::AfterStop) => ensure!(
                self.automatic_signals(&workspace.id, snapshot.rule, cancelled)
                    .await?
                    == Some(snapshot.signals),
                "workspace changed {}",
                stage.change()
            ),
            // The tree is compared only after the hooks: change times only
            // move forward, so that also covers changes made before them.
            (Removal::Automatic { snapshot, .. }, Stage::AfterHooks) => ensure!(
                self.automatic_snapshot(&workspace.id, snapshot.rule, cancelled)
                    .await?
                    == Some(snapshot),
                "workspace changed {}",
                stage.change()
            ),
            // The workspaces below go first, under the same completion, so
            // none that something keeps goes with this one.
            (Removal::UnderCompleted { ancestor }, _) => {
                ensure!(
                    check.workspace.children.is_empty(),
                    "spaces below it remain; retaining it until they go"
                );
                // A chat's worker may have been opened elsewhere since; the
                // reservation keeps it where it is from here on.
                let nearest = self.completed_ancestor(&check.workspace.id).await?;
                ensure!(
                    nearest.is_some_and(|nearest| nearest.id() == ancestor.id()),
                    "{} withdrew its completion, changed or no longer has the workspace below it; retaining workspace",
                    ancestor.name()
                );
            }
            (Removal::Manual { .. } | Removal::Deleted | Removal::Parent { .. }, _) => {}
        }
        Ok(check)
    }

    /// Refuse a removal that [skips stalled](Removal::skips_stalled)
    /// workspaces while one of the `children` of the space it removes, or
    /// one below them, which may be in another directory, is in a directory
    /// whose file access stalled. The sweep's own pass of that workspace
    /// rechecks it.
    pub(super) async fn ensure_none_stalled_below(
        &self,
        children: Vec<SpaceRef>,
        removal: Removal<'_>,
    ) -> Result<()> {
        if !removal.skips_stalled() {
            return Ok(());
        }
        let mut pending = children;
        while let Some(child) = pending.pop() {
            // A chat has no directory of its own; those below it may.
            if child.kind == SpaceKind::Workspace {
                let workspace = self.workspace_by_id(&child.id).await?;
                ensure!(
                    self.file_access.stalled(&workspace.path).is_none(),
                    "file access stalled in the directory of {}, below it; retaining workspace",
                    child.name
                );
            }
            pending.extend(self.taken_first(&child.id).await?);
        }
        Ok(())
    }

    /// The directory is gone: a worktree that was deleted rather than moved is
    /// forgotten, and its branch is always retained.
    async fn remove_missing_worktree(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            !matches!(removal, Removal::Automatic { .. }),
            "workspace directory disappeared during automatic cleanup"
        );
        ensure!(
            self.missing_worktree(workspace).await?.is_none(),
            "worktree was moved; restore its recorded path before removing it"
        );
        if matches!(removal, Removal::Deleted) {
            ensure!(
                !self.workspace_has_executions(&workspace.id).await?,
                "commands are recorded; stop them with shoal stop or shoal rm"
            );
        }
        let below = self.taken_first(&workspace.id).await?;
        self.ensure_none_stalled_below(below, removal).await?;
        self.stop_executions(
            &workspace.id,
            removal.stop_policy(),
            &StopRequest::removal(),
        )
        .await?;
        self.stop_removed_processes(workspace, WorktreeTies::None)
            .await?;
        let children = self.remove_workspace_children(workspace, removal).await?;
        // Prune only this owned registration, through Worktrunk, and retain
        // its branch because the contents cannot be inspected.
        let registered = self.is_registered_worktree(workspace).await?;
        let removed = self
            .remove_worktree(
                &repo.path,
                workspace,
                FileRemoval::Force,
                BranchRemoval::Keep,
                registered,
            )
            .await?;
        Ok(RemovalResult {
            children,
            ..removed
        })
    }

    /// [`Self::remove_children`] for the chats in the worktree of
    /// `workspace` and the spaces below it, listed once it was reserved, so
    /// no chat starts there and no child follows.
    async fn remove_workspace_children(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<Vec<SpaceName>> {
        let children = self.taken_first(&workspace.id).await?;
        self.remove_children(&workspace.name, children, removal)
            .await
    }

    /// Remove `children`, the spaces the removal of the space named `parent`
    /// takes first, for `removal` of it, each after those below it,
    /// returning their names in that order. The first that fails stops the
    /// rest and retains the parent, so no space outlives its parent. Each
    /// reservation refuses children below its space and chats in its
    /// worktree, so none appears in one whose children were listed.
    pub(super) async fn remove_children(
        &self,
        parent: &str,
        children: Vec<SpaceRef>,
        removal: Removal<'_>,
    ) -> Result<Vec<SpaceName>> {
        ensure_confirmed(parent, &children, removal)?;
        let mut removed = Vec::new();
        for child in children {
            let below = match child.kind {
                SpaceKind::Workspace => {
                    Box::pin(self.remove(&SpaceSelector::id(&child.id), removal.of_child()))
                        .await
                        .map(|result| result.children)
                }
                SpaceKind::Chat => match self.chat_by_id(&child.id).await? {
                    Some(chat) => Box::pin(self.remove_chat_for(&chat, removal.of_child())).await,
                    None => continue,
                },
            };
            let below = below.with_context(|| {
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

    /// Release live resources immediately before removing the worktree. Failed
    /// cleanup retains their ownership records so removal can be retried.
    async fn remove_worktree(
        &self,
        repo: &Path,
        workspace: &Workspace,
        files: FileRemoval,
        branch: BranchRemoval,
        registered: bool,
    ) -> Result<RemovalResult> {
        self.remove_simulators(&workspace.id).await?;
        if registered {
            worktrunk::remove(
                repo,
                &self.paths.worktrunk_config(),
                &workspace.path,
                files,
                branch,
            )
            .await
        } else {
            Ok(RemovalResult {
                removed: true,
                branch: workspace.branch.clone(),
                branch_outcome: BranchOutcome::Retained,
                hook_error: None,
                children: Vec::new(),
            })
        }
    }
}

/// Refuse to delete the unsaved files of the worktree `check` inspected
/// when `removal` discards them or promises to keep them restorable, unless
/// the checkpoint of `record` keeps them.
fn ensure_checkpoint_keeps_files(
    check: &Inspection,
    removal: Removal<'_>,
    record: Option<&RemovedWorkspace>,
) -> Result<()> {
    if check.dirty
        && (removal.discards_archived_files() || removal.unsaved().promises_restorable_files())
    {
        ensure!(
            record.is_some_and(|record| !record.checkpoint.files_omitted),
            "a checkpoint cannot keep the unsaved files; retaining the workspace"
        );
    }
    Ok(())
}

/// Worktrunk's options for the worktree the final inspection `check` saw.
/// A branch goes when its commits are all preserved, except the default
/// branch, which an unresolved default branch could be; files go by force
/// only once `removal` may discard unsaved work, or discards unsaved files a
/// checkpoint keeps. A workspace at a commit owns no branch to delete,
/// whichever its worktree is on. A disposable workspace's branch counts as
/// preserved while the worktree is still on it, and its files always go by
/// force.
pub(super) fn worktrunk_removal_options(
    check: &Inspection,
    removal: Removal<'_>,
) -> (FileRemoval, BranchRemoval) {
    let own_branch = check.workspace.branch.is_some()
        && (!check.workspace.disposable || check.branch == check.workspace.branch);
    let branch = if own_branch
        && check.branch_preserved()
        && check
            .default_branch
            .as_deref()
            .is_some_and(|default| check.branch.as_deref() != Some(default))
    {
        BranchRemoval::Delete
    } else {
        BranchRemoval::Keep
    };
    // Worktrunk refuses a changed worktree unless forced, even when the
    // changes are only what a copy started with.
    let files = if check.workspace.disposable
        || removal.unsaved().deletes_unsaved()
        || check.starting_files
        || (check.dirty && removal.discards_archived_files())
    {
        FileRemoval::Force
    } else {
        FileRemoval::CleanOnly
    };
    (files, branch)
}
