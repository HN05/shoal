//! The worktree's part of a removal: what it checks of the worktree's Git
//! state, the processes and hooks it stops and runs there, and deleting the
//! worktree once a checkpoint keeps its work, or forgetting one that was
//! deleted.
use super::super::{Removal, STOPPED_REMOVAL, Stage};
use super::candidacy::Baseline;
use crate::daemon::runner::JobStop;
use crate::daemon::space_processes::WorktreeTies;
use crate::daemon::workspace::ownership::{CommonDir, verify_admin_dir_owner};
use crate::daemon::{executions::StopRequest, manager::Manager};
use crate::model::EventCause;
use crate::{
    fsutil::off_runtime,
    git::{
        self,
        worktree::{BranchRemoval, FileRemoval, interrupted},
    },
    hooks::{self, Hook, HookKind},
    model::{CheckpointCause, RemovedSpace, Space, Workspace},
    removal::{
        self, BranchOutcome, IdleRestore, Inspection, InspectionPolicy, RemovalCheck, RemovalResult,
    },
    state::WorkspaceState,
};
use anyhow::{Result, bail, ensure};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// A post-remove hook resolved while the workspace record still exists.
struct PostRemoveHook {
    command: PathBuf,
    checkout: PathBuf,
    env: BTreeMap<String, OsString>,
}

/// What Git records of a worktree being removed.
#[derive(Debug, Clone, Copy)]
enum Registration {
    /// Git lists it, so Git removes it.
    Listed,
    /// Its directory stays though Git no longer lists it, so removal
    /// deletes what is left once a checkpoint keeps what is worth keeping.
    Disowned,
    /// Git no longer lists it, and its directory is gone.
    Unlisted,
}

/// The worktree a removal takes, as the removal found it once it reserved
/// the workspace.
pub(in crate::daemon) struct WorktreeStage {
    workspace: Workspace,
    /// Whether its directory existed; a missing one is forgotten instead.
    present: bool,
    post_remove: Option<PostRemoveHook>,
    xcode: Option<crate::config::Effective>,
    /// What each inspection compares an existing worktree with.
    baseline: Option<Baseline>,
}

/// What removing a worktree did.
#[derive(Debug, Clone)]
pub(crate) struct WorktreeRemoval {
    pub(crate) removed: bool,
    pub(crate) branch: Option<BranchName>,
    pub(crate) branch_outcome: BranchOutcome,
    /// The removal succeeded, but its best-effort post-remove hook failed.
    pub(crate) hook_error: Option<String>,
}

impl WorktreeRemoval {
    /// The protocol's result of the workspace removal that took `children`
    /// first.
    pub(crate) fn with_children(
        self,
        children: Vec<shoal_protocol::space_names::SpaceName>,
    ) -> RemovalResult {
        RemovalResult {
            removed: self.removed,
            branch: self.branch,
            branch_outcome: self.branch_outcome,
            hook_error: self.hook_error,
            children,
        }
    }
}

impl From<RemovalResult> for WorktreeRemoval {
    fn from(result: RemovalResult) -> Self {
        Self {
            removed: result.removed,
            branch: result.branch,
            branch_outcome: result.branch_outcome,
            hook_error: result.hook_error,
        }
    }
}

impl WorktreeStage {
    /// What the journal records as the removal's cause: `removal`'s, or the
    /// deleted directory's, which a worktree yet to be created never had.
    pub(in crate::daemon) fn cause(&self, removal: Removal<'_>) -> EventCause {
        if self.present || self.workspace.checkout.unborn() {
            removal.cause()
        } else {
            EventCause::MissingDirectory
        }
    }

    /// What ties processes to the workspace as `removal` stops them: nothing
    /// for a deleted directory.
    pub(in crate::daemon) fn ties(&self, removal: Removal<'_>) -> WorktreeTies {
        match self.present {
            true => removal.worktree_ties(),
            false => WorktreeTies::None,
        }
    }

    /// The shared Git directory of the repository of an existing worktree,
    /// as the removal read it.
    pub(in crate::daemon) fn common_dir(&self) -> Option<&CommonDir> {
        self.baseline.as_ref().map(Baseline::common_dir)
    }

    /// How the removal stops the workspace's commands: those of a deleted
    /// directory cannot be restored there.
    pub(in crate::daemon) fn stop_request(&self) -> StopRequest {
        if self.present {
            StopRequest::restorable_removal()
        } else {
            StopRequest::removal()
        }
    }
}

impl Manager {
    /// What removing the worktree of `workspace` at a request would discard,
    /// with its changed files and whether removal archives them when
    /// `include_changes` is set and it has any.
    pub(in crate::daemon) async fn check_manual_worktree(
        &self,
        workspace: &Workspace,
        include_changes: bool,
    ) -> Result<RemovalCheck> {
        let selector = SpaceSelector::id(&workspace.id);
        let mut check = (self
            .check_removal(&selector, InspectionPolicy::GitOnly)
            .await?)
            .check;
        if include_changes && check.dirty {
            let repo = self.repositories.repository(workspace.repository()).await?;
            removal::load_changed_files(&mut check, &repo.path).await?;
            check.unsaved_files_archived = self
                .checkpoint_keeps_unsaved_files(&check.workspace, CheckpointCause::Removal)
                .await?;
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

    /// Begin the worktree's part of `removal` of the reserved `workspace`,
    /// which was `replaced` before: settle an earlier removal that stopped
    /// partway, then resolve the post-remove hook and Xcode data while its
    /// record exists.
    pub(in crate::daemon) async fn begin_worktree_removal(
        &self,
        workspace: &Workspace,
        replaced: WorkspaceState,
        removal: Removal<'_>,
    ) -> Result<WorktreeStage> {
        let present = off_runtime::try_exists(&workspace.path).await?;
        if matches!(removal, Removal::Deleted) && present {
            bail!("workspace directory exists");
        }
        let (post_remove, baseline) = if present {
            self.ensure_no_pending_rename(&workspace.id).await?;
            self.settle_interrupted_removal(workspace, replaced).await?;
            (
                self.prepare_post_remove_hook(workspace).await?,
                Some(self.inspection_baseline(workspace).await?),
            )
        } else {
            (None, None)
        };
        Ok(WorktreeStage {
            xcode: self.xcode_settings(workspace, present).await,
            workspace: workspace.clone(),
            present,
            post_remove,
            baseline,
        })
    }

    /// Settle an earlier removal of `workspace`, in lifecycle state `state`,
    /// that stopped before Git finished and did not put back what it
    /// deleted, as when the daemon stopped meanwhile. Such a removal leaves
    /// the workspace failed with a [`STOPPED_REMOVAL`] error, so that its
    /// `.git` link and tracked files come back before an inspection takes
    /// their deletion for unsaved work. A workspace something readied or
    /// failed otherwise since has its marker dropped instead: the deletions
    /// there may be its agent's. Only the recorded admin directory, marked
    /// as this workspace's, says that a removal began.
    pub(in crate::daemon) async fn settle_interrupted_removal(
        &self,
        workspace: &Workspace,
        state: WorkspaceState,
    ) -> Result<()> {
        let Some(admin_dir) = workspace.checkout.git_dir() else {
            return Ok(());
        };
        if !interrupted::began(admin_dir).await? {
            return Ok(());
        }
        if !left_by_stopped_removal(workspace, state) {
            return interrupted::forget(admin_dir).await;
        }
        verify_admin_dir_owner(workspace, admin_dir).await?;
        if interrupted::restore(&workspace.path, admin_dir).await? {
            crate::daemon::log!(
                "restored the files an interrupted removal of {} deleted",
                workspace.name
            );
        }
        Ok(())
    }

    /// What a `removal` of `workspace`, which was `replaced` before, that
    /// failed with `error` leaves: whether the workspace can no longer be
    /// ready, with the error to record. Its directory was deleted, or a
    /// [`STOPPED_REMOVAL`] left what it deleted for the next removal to put
    /// back, which its error says: this removal's `worktree remove`, stopped
    /// partway and unable to restore, or an earlier one whose marker this
    /// removal failed before settling.
    pub(in crate::daemon) async fn failed_worktree_removal(
        &self,
        workspace: &Workspace,
        replaced: WorkspaceState,
        removal: Removal<'_>,
        error: anyhow::Error,
    ) -> (bool, anyhow::Error) {
        if error.downcast_ref::<interrupted::Unrestored>().is_some()
            || stopped_removal_pending(workspace, replaced).await
        {
            return (true, error.context(format!("{STOPPED_REMOVAL} partway")));
        }
        let deleted = matches!(removal, Removal::Deleted)
            && !matches!(off_runtime::try_exists(&workspace.path).await, Ok(true));
        (deleted, error)
    }

    /// Inspect the worktree again and apply every check `removal` needs of it
    /// at `stage`, after stopping the `cancelled` restores, returning the
    /// inspection removal proceeds with; none for a deleted directory, whose
    /// checks come before the removal stops anything.
    pub(in crate::daemon::lifecycle) async fn check_worktree_removal(
        &self,
        worktree: &WorktreeStage,
        removal: Removal<'_>,
        stage: Stage,
        cancelled: &[IdleRestore],
    ) -> Result<Option<Inspection>> {
        let workspace = &worktree.workspace;
        let Some(baseline) = &worktree.baseline else {
            if matches!(stage, Stage::Initial) {
                self.check_missing_worktree(workspace, removal).await?;
            }
            return Ok(None);
        };
        (self.check_worktree(workspace, removal, stage, cancelled, baseline))
            .await
            .map(Some)
    }

    /// Inspect the worktree of `workspace` against `baseline` and apply
    /// every check `removal` needs of it at `stage`, after stopping
    /// the `cancelled` restores. A deleted directory inspects as clean,
    /// while its HEAD cannot be read.
    pub(in crate::daemon::lifecycle) async fn check_worktree(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
        stage: Stage,
        cancelled: &[IdleRestore],
        baseline: &Baseline,
    ) -> Result<Inspection> {
        let executions = self.cleanup_executions(&workspace.id, cancelled).await?;
        let check = self
            .inspect_removal_against(
                self.workspace_by_id(&workspace.id).await?,
                removal.inspection(),
                executions,
                baseline,
            )
            .await?;
        removal.verify(&check, stage)?;
        // What the removal hooks changed only this machine has.
        if let (Some(moved), Stage::Final) = (removal.moved(), stage) {
            self.ensure_moved_workspace_unchanged(workspace, moved)
                .await?;
        }
        if let Removal::Merged { head } | Removal::Completed { head, .. } = removal {
            ensure!(
                shoal_links::pr::current_head(workspace).await? == head,
                "HEAD changed {}",
                stage.change()
            );
        }
        // Its record stays, as a chat's, so nothing may build on its branch,
        // which goes with the worktree, whenever that began.
        if removal.converts()
            && let Some(stacked) = check.workspace.stacked_workspaces.first()
        {
            bail!(
                "workspace {} builds on its branch; retaining workspace",
                stacked.name
            );
        }
        Ok(check)
    }

    /// Refuse to forget a deleted worktree on `removal`: automatic cleanup
    /// takes only unused ones, a moved one is restored to its recorded path
    /// instead, and commands recorded there could still run.
    async fn check_missing_worktree(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<()> {
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
        Ok(())
    }

    /// End what the worktree runs that its commands' stop leaves alone: its
    /// review goes with its record, and a CI job ends through its forge
    /// before its runner is stopped.
    pub(in crate::daemon) async fn stop_worktree_work(&self, worktree: &WorktreeStage) {
        self.reviews
            .stop_in_workspace(self, &worktree.workspace.id)
            .await;
        (self.runner)
            .cancel_job_before_stopping(self, &worktree.workspace.id, JobStop::Removal)
            .await;
    }

    /// Run the worktree's removal hooks, which a deleted directory does not.
    pub(in crate::daemon) async fn run_removal_hooks(
        &self,
        worktree: &WorktreeStage,
    ) -> Result<()> {
        if !worktree.present {
            return Ok(());
        }
        self.run_pre_remove_hook(&worktree.workspace).await?;
        self.run_resource_release_hooks(&worktree.workspace).await
    }

    /// Delete the worktree as the final inspection `check` decides, once a
    /// checkpoint keeps its HEAD and the unsaved files `removal` discards or
    /// promises to keep, or forget a deleted one, retaining its branch since
    /// its contents cannot be inspected. Its containers, simulators and Xcode
    /// data go with it.
    pub(in crate::daemon) async fn delete_worktree(
        &self,
        worktree: &WorktreeStage,
        removal: Removal<'_>,
        check: Option<&Inspection>,
    ) -> Result<WorktreeRemoval> {
        let workspace = &worktree.workspace;
        let repo = self.repositories.repository(workspace.repository()).await?;
        let removed = match check {
            Some(check) => {
                self.delete_checkpointed_worktree(&repo.path, workspace, removal, check)
                    .await?
            }
            None => {
                // Prune only this owned registration.
                let registration = match self.is_registered_worktree(workspace).await? {
                    true => Registration::Listed,
                    false => Registration::Unlisted,
                };
                (self.remove_worktree(
                    &repo.path,
                    workspace,
                    FileRemoval::Force,
                    BranchRemoval::Keep,
                    registration,
                ))
                .await?
            }
        };
        // While the record still reserves the path: a workspace created
        // there gets the same DerivedData.
        if let Some(settings) = &worktree.xcode {
            self.delete_xcode_data(workspace, settings).await;
        }
        Ok(removed)
    }

    /// What only a removed worktree leaves behind once the record is gone:
    /// the branch it kept, and its post-remove hook, which runs with the
    /// workspace's identity.
    pub(in crate::daemon) async fn complete_worktree_removal(
        &self,
        worktree: WorktreeStage,
        mut outcome: WorktreeRemoval,
    ) -> WorktreeRemoval {
        let workspace = &worktree.workspace;
        if let Err(error) = self.remember_kept_branch(workspace, &outcome).await {
            crate::daemon::log!(
                "kept branch of {} not remembered: {error:#}",
                workspace.name
            );
        }
        if let Some(hook) = worktree.post_remove {
            outcome.hook_error = self.run_post_remove_hook(workspace, &hook).await;
            let space = Space::from(workspace.clone());
            if let Err(error) = self
                .stop_removed_processes(&space, WorktreeTies::None, None)
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

    /// The removal hook `kind` of `workspace`; none for one whose setup was
    /// skipped, since they tear down what setup made.
    async fn removal_hook(&self, workspace: &Workspace, kind: HookKind) -> Result<Option<PathBuf>> {
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
                checkout: self
                    .repositories
                    .repository(workspace.repository())
                    .await?
                    .path,
                env: self.removed_workspace_environment(workspace).await?,
            })),
            None => Ok(None),
        }
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
        self.coordination
            .notify_space(
                self,
                &workspace.name,
                &workspace.id,
                crate::daemon::notifications::AlertKind::HookFailed,
                &message,
            )
            .await;
        Some(message)
    }

    /// Run the pre-remove hook, which sees the worktree intact; a failing
    /// hook retains the workspace.
    async fn run_pre_remove_hook(&self, workspace: &Workspace) -> Result<()> {
        let Some(command) = self.removal_hook(workspace, HookKind::PreRemove).await? else {
            return Ok(());
        };
        let settings = self
            .repositories
            .workspace_settings(self, workspace)
            .await?;
        let repository = workspace.repository();
        let configured_env =
            (self.hook_environment(workspace, Some(repository), &settings)).await?;
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
            .leases
            .list_resources(self, Some(&SpaceSelector::id(&workspace.id)))
            .await?;
        if leases.is_empty() {
            return Ok(());
        }
        let release_hook = self
            .workspace_hook(workspace, HookKind::PreResourceRelease)
            .await?;
        for lease in leases {
            self.leases
                .run_resource_release_hook(self, workspace, &lease, release_hook.as_deref())
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
    ) -> Result<WorktreeRemoval> {
        let record = self.save_restore_record(workspace, removal).await?;
        ensure_checkpoint_keeps_files(check, removal, record.as_ref())?;
        let built_on = self.branch_built_on(workspace).await?;
        let (files, branch) = removal_options(check, removal, built_on);
        let registration = self.existing_registration(workspace, repo).await?;
        self.remove_worktree(repo, workspace, files, branch, registration)
            .await
    }

    /// What Git records of the existing worktree of `workspace` that the
    /// removal deletes: disowned, or listed once the identity of its
    /// directory is recorded, which proves what Git leaves there should the
    /// deletion stop partway.
    async fn existing_registration(
        &self,
        workspace: &Workspace,
        repo: &Path,
    ) -> Result<Registration> {
        if self.disowned(workspace, repo).await? {
            return Ok(Registration::Disowned);
        }
        self.record_removal_directory(workspace).await?;
        Ok(Registration::Listed)
    }

    /// The record that restores the workspace, with a checkpoint of its HEAD
    /// and unsaved files; a disposable workspace keeps nothing to restore.
    async fn save_restore_record(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
    ) -> Result<Option<RemovedSpace>> {
        if workspace.disposable {
            return Ok(None);
        }
        let carried_on = match removal {
            Removal::Converted { chat } => Some(&chat.execution),
            _ => None,
        };
        self.save_removal_record(workspace, removal.cause(), carried_on)
            .await
            .map(Some)
    }

    /// Release live resources immediately before removing the worktree, its
    /// containers first. Failed cleanup retains their ownership records so
    /// removal can be retried.
    /// Git cannot tell which branch a worktree it no longer lists was on, so
    /// the recorded one stays.
    async fn remove_worktree(
        &self,
        repo: &Path,
        workspace: &Workspace,
        files: FileRemoval,
        branch: BranchRemoval,
        registration: Registration,
    ) -> Result<WorktreeRemoval> {
        self.leases.remove_containers(workspace).await?;
        self.leases.remove_simulators(&workspace.id).await?;
        match registration {
            Registration::Listed => {
                self.repositories
                    .destroy_seed_outputs(self, workspace)
                    .await;
                let removed = git::worktree::remove(repo, &workspace.path, files, branch).await?;
                return Ok(removed.into());
            }
            Registration::Disowned => git::worktree::delete_disowned(&workspace.path).await?,
            Registration::Unlisted => {}
        }
        Ok(WorktreeRemoval {
            removed: true,
            branch: workspace.branch().cloned(),
            branch_outcome: BranchOutcome::Retained,
            hook_error: None,
        })
    }

    /// Refuse a removal that [skips stalled](Removal::skips_stalled) spaces
    /// while the worktree of `workspace`, below the space it removes, is in a
    /// directory whose file access stalled. The sweep's own pass of that
    /// workspace rechecks it.
    pub(in crate::daemon) fn ensure_worktree_answers(&self, workspace: &Workspace) -> Result<()> {
        ensure!(
            self.file_access.stalled(&workspace.path).is_none(),
            "file access stalled in the directory of {}, below it; retaining it",
            workspace.name
        );
        Ok(())
    }
}

/// Whether `workspace`, in lifecycle state `state`, still waits for the
/// files a stopped removal deleted: that removal left it, and its marker
/// stays. A marker that cannot be read counts too.
async fn stopped_removal_pending(workspace: &Workspace, state: WorkspaceState) -> bool {
    let Some(admin_dir) = workspace.checkout.git_dir() else {
        return false;
    };
    left_by_stopped_removal(workspace, state)
        && !matches!(interrupted::began(admin_dir).await, Ok(false))
}

/// Whether `workspace`, in lifecycle state `state`, is as a removal that
/// stopped partway left it: failed with that removal's error, which every
/// other failure, and readying it, replaces.
fn left_by_stopped_removal(workspace: &Workspace, state: WorkspaceState) -> bool {
    state == WorkspaceState::Failed
        && (workspace.error.as_deref()).is_some_and(|error| error.starts_with(STOPPED_REMOVAL))
}

/// Refuse to delete the unsaved files of the worktree `check` inspected
/// when `removal` discards them or promises to keep them restorable, unless
/// the checkpoint of `record` keeps them.
fn ensure_checkpoint_keeps_files(
    check: &Inspection,
    removal: Removal<'_>,
    record: Option<&RemovedSpace>,
) -> Result<()> {
    if check.dirty
        && (removal.discards_archived_files() || removal.unsaved().promises_restorable_files())
    {
        ensure!(
            (record.and_then(RemovedSpace::workspace))
                .is_some_and(|kept| !kept.checkpoint.files_omitted),
            "a checkpoint cannot keep the unsaved files; retaining the workspace"
        );
    }
    Ok(())
}

/// What removal takes with the worktree the final inspection `check` saw.
/// A branch goes when its commits are all preserved, except the default
/// branch, which an unresolved default branch could be, and a branch that
/// another workspace records building on, `built_on`; files go by force
/// only once `removal` may discard unsaved work, or discards unsaved files a
/// checkpoint keeps. A workspace at a commit owns no branch to delete,
/// whichever its worktree is on. A disposable workspace's branch counts as
/// preserved while the worktree is still on it, and its files always go by
/// force.
pub(in crate::daemon) fn removal_options(
    check: &Inspection,
    removal: Removal<'_>,
    built_on: bool,
) -> (FileRemoval, BranchRemoval) {
    let own_branch = check.workspace.branch().is_some()
        && (!check.workspace.disposable || check.branch.as_ref() == check.workspace.branch());
    let branch = if own_branch
        && !removal.converts()
        && check.branch_preserved()
        && !built_on
        && check
            .default_branch
            .as_deref()
            .is_some_and(|default| check.branch.as_ref().map(BranchName::as_str) != Some(default))
    {
        BranchRemoval::Delete
    } else {
        BranchRemoval::Keep
    };
    // Git refuses a changed worktree unless forced, even when the changes
    // are only what a copy started with.
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
