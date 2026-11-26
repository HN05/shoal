//! The worktree's part of a removal: checking its Git state, stopping and
//! running what runs there, and deleting the worktree once a checkpoint
//! keeps its work, or forgetting one that was deleted.
use super::super::{Removal, STOPPED_REMOVAL, Stage};
use super::candidacy::Baseline;
use crate::Spaces;
use crate::daemon::Daemon;
use crate::jobs::JobStop;
use crate::model::EventCause;
use crate::space_processes::WorktreeTies;
use crate::stops::StopRequest;
use crate::workspace::ownership::{CommonDir, verify_admin_dir_owner};
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

/// Resolved while the workspace record still exists.
struct PostRemoveHook {
    command: PathBuf,
    checkout: PathBuf,
    env: BTreeMap<String, OsString>,
}

#[derive(Debug, Clone, Copy)]
enum Registration {
    /// Git lists it, so Git removes it.
    Listed,
    /// Its directory stays though Git no longer lists it, so removal deletes
    /// what is left once a checkpoint keeps what is worth keeping.
    Disowned,
    /// Git no longer lists it, and its directory is gone.
    Unlisted,
}

/// As the removal found it once it reserved the workspace.
pub struct WorktreeStage {
    workspace: Workspace,
    /// A missing directory is forgotten instead.
    present: bool,
    post_remove: Option<PostRemoveHook>,
    xcode: Option<crate::config::Effective>,
    /// What each inspection compares an existing worktree with.
    baseline: Option<Baseline>,
}

#[derive(Debug, Clone)]
pub struct WorktreeRemoval {
    pub removed: bool,
    pub branch: Option<BranchName>,
    pub branch_outcome: BranchOutcome,
    /// The removal succeeded, but its best-effort post-remove hook failed.
    pub hook_error: Option<String>,
}

impl WorktreeRemoval {
    pub fn with_children(
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
    /// `removal`'s cause, or the deleted directory's, which a worktree yet to
    /// be created never had.
    pub fn cause(&self, removal: Removal<'_>) -> EventCause {
        if self.present || self.workspace.checkout.unborn() {
            removal.cause()
        } else {
            EventCause::MissingDirectory
        }
    }

    /// Nothing for a deleted directory.
    pub fn ties(&self, removal: Removal<'_>) -> WorktreeTies {
        match self.present {
            true => removal.worktree_ties(),
            false => WorktreeTies::None,
        }
    }

    /// As the removal read it.
    pub fn common_dir(&self) -> Option<&CommonDir> {
        self.baseline.as_ref().map(Baseline::common_dir)
    }

    /// The commands of a deleted directory cannot be restored there.
    pub fn stop_request(&self) -> StopRequest {
        if self.present {
            StopRequest::restorable_removal()
        } else {
            StopRequest::removal()
        }
    }
}

impl Spaces {
    /// With `include_changes`, also returns its changed files and whether
    /// removal archives them.
    pub async fn check_manual_worktree(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        include_changes: bool,
    ) -> Result<RemovalCheck> {
        let selector = SpaceSelector::id(&workspace.id);
        let mut check = (self
            .check_removal(daemon, &selector, InspectionPolicy::GitOnly)
            .await?)
            .check;
        if include_changes && check.dirty {
            let repo = daemon
                .repository(crate::repository::Selector::from(workspace.repository()))
                .await?;
            removal::load_changed_files(&mut check, &repo.path).await?;
            check.unsaved_files_archived = self
                .checkpoint_keeps_unsaved_files(daemon, &check.workspace, CheckpointCause::Removal)
                .await?;
        }
        Ok(check)
    }

    pub async fn check_removal(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        policy: InspectionPolicy,
    ) -> Result<Inspection> {
        let workspace = self.workspace(selector).await?;
        let executions = self.cleanup_executions(daemon, &workspace.id, &[]).await?;
        self.inspect_removal(daemon, workspace, policy, executions)
            .await
    }

    /// Settles an earlier removal that stopped partway, then resolves the
    /// post-remove hook and Xcode data while the record exists. `replaced`
    /// is the state the reservation replaced.
    pub async fn begin_worktree_removal(
        &self,
        daemon: &dyn Daemon,
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
                self.prepare_post_remove_hook(daemon, workspace).await?,
                Some(self.inspection_baseline(daemon, workspace).await?),
            )
        } else {
            (None, None)
        };
        Ok(WorktreeStage {
            xcode: self.xcode_settings(daemon, workspace, present).await,
            workspace: workspace.clone(),
            present,
            post_remove,
            baseline,
        })
    }

    /// Settles an earlier removal that stopped before Git finished and did
    /// not put back what it deleted, as when the daemon stopped meanwhile.
    /// Such a removal leaves the workspace failed with a
    /// [`STOPPED_REMOVAL`] error, so its `.git` link and tracked files come
    /// back before an inspection takes their deletion for unsaved work. A
    /// workspace something readied or failed otherwise since has its marker
    /// dropped instead: the deletions there may be its agent's. Only the
    /// recorded admin directory, marked as this workspace's, says that a
    /// removal began.
    pub async fn settle_interrupted_removal(
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
            crate::log!(
                "restored the files an interrupted removal of {} deleted",
                workspace.name
            );
        }
        Ok(())
    }

    /// Whether the workspace can no longer be ready, with the error to
    /// record. Its directory was deleted, or a [`STOPPED_REMOVAL`] left what
    /// it deleted for the next removal to put back, which its error says:
    /// this removal's `worktree remove`, stopped partway and unable to
    /// restore, or an earlier one whose marker this removal failed before
    /// settling.
    pub async fn failed_worktree_removal(
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

    /// Inspects again, disregarding the `cancelled` restores the caller
    /// already stopped. None for a deleted directory, whose checks come
    /// before the removal stops anything.
    pub(in crate::lifecycle) async fn check_worktree_removal(
        &self,
        daemon: &dyn Daemon,
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
        (self.check_worktree(daemon, workspace, removal, stage, cancelled, baseline))
            .await
            .map(Some)
    }

    /// Disregards the `cancelled` restores the caller already stopped. A
    /// deleted directory inspects as clean, while its HEAD cannot be read.
    pub(in crate::lifecycle) async fn check_worktree(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        removal: Removal<'_>,
        stage: Stage,
        cancelled: &[IdleRestore],
        baseline: &Baseline,
    ) -> Result<Inspection> {
        let executions = self
            .cleanup_executions(daemon, &workspace.id, cancelled)
            .await?;
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
            self.ensure_moved_workspace_unchanged(daemon, workspace, moved)
                .await?;
        }
        if let Removal::Merged { head } | Removal::Completed { head, .. } = removal {
            ensure!(
                daemon.current_head(workspace).await? == head,
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

    /// Automatic cleanup takes only unused ones, a moved one is restored to
    /// its recorded path instead, and commands recorded there could still
    /// run.
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

    /// Ends what the commands' stop leaves alone: the review goes with the
    /// record, and a CI job ends through its forge before its runner stops.
    pub async fn stop_worktree_work(&self, daemon: &dyn Daemon, worktree: &WorktreeStage) {
        daemon.stop_reviews(&worktree.workspace.id).await;
        daemon
            .cancel_job_before_stopping(&worktree.workspace.id, JobStop::Removal)
            .await;
    }

    /// A deleted directory runs none.
    pub async fn run_removal_hooks(
        &self,
        daemon: &dyn Daemon,
        worktree: &WorktreeStage,
    ) -> Result<()> {
        if !worktree.present {
            return Ok(());
        }
        self.run_pre_remove_hook(daemon, &worktree.workspace)
            .await?;
        self.run_resource_release_hooks(daemon, &worktree.workspace)
            .await
    }

    /// Deletes once a checkpoint keeps HEAD and the unsaved files `removal`
    /// discards or promises to keep. A deleted worktree is forgotten, and
    /// its branch kept, since its contents cannot be inspected. Containers,
    /// simulators and Xcode data go with it.
    pub async fn delete_worktree(
        &self,
        daemon: &dyn Daemon,
        worktree: &WorktreeStage,
        removal: Removal<'_>,
        check: Option<&Inspection>,
    ) -> Result<WorktreeRemoval> {
        let workspace = &worktree.workspace;
        let repo = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        let removed = match check {
            Some(check) => {
                self.delete_checkpointed_worktree(daemon, &repo.path, workspace, removal, check)
                    .await?
            }
            None => {
                // Prune only this owned registration.
                let registration = match self.is_registered_worktree(daemon, workspace).await? {
                    true => Registration::Listed,
                    false => Registration::Unlisted,
                };
                (self.remove_worktree(
                    daemon,
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
            self.delete_xcode_data(daemon, workspace, settings).await;
        }
        Ok(removed)
    }

    /// Runs once the record is gone: records the branch the removal kept for
    /// cleanup to delete once its commits are elsewhere, and runs the
    /// post-remove hook with the workspace's identity.
    pub async fn complete_worktree_removal(
        &self,
        daemon: &dyn Daemon,
        worktree: WorktreeStage,
        mut outcome: WorktreeRemoval,
    ) -> WorktreeRemoval {
        let workspace = &worktree.workspace;
        if let Err(error) = self.remember_kept_branch(daemon, workspace, &outcome).await {
            crate::log!(
                "kept branch of {} not remembered: {error:#}",
                workspace.name
            );
        }
        if let Some(hook) = worktree.post_remove {
            outcome.hook_error = self.run_post_remove_hook(daemon, workspace, &hook).await;
            let space = Space::from(workspace.clone());
            if let Err(error) = self
                .stop_removed_processes(daemon, &space, WorktreeTies::None, None)
                .await
            {
                crate::log!(
                    "processes left by the post-remove hook of {} not stopped: {error:#}",
                    workspace.name
                );
            }
        }
        outcome
    }

    /// None for a workspace whose setup was skipped, since the hooks tear
    /// down what setup made.
    async fn removal_hook(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        kind: HookKind,
    ) -> Result<Option<PathBuf>> {
        if workspace.setup_skipped {
            return Ok(None);
        }
        self.workspace_hook(daemon, workspace, kind).await
    }

    async fn prepare_post_remove_hook(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<Option<PostRemoveHook>> {
        match self
            .removal_hook(daemon, workspace, HookKind::PostRemove)
            .await?
        {
            Some(command) => Ok(Some(PostRemoveHook {
                command,
                checkout: daemon
                    .repository(crate::repository::Selector::from(workspace.repository()))
                    .await?
                    .path,
                env: self
                    .removed_workspace_environment(daemon, workspace)
                    .await?,
            })),
            None => Ok(None),
        }
    }

    /// Notifies on failure and returns it.
    async fn run_post_remove_hook(
        &self,
        daemon: &dyn Daemon,
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
        daemon
            .coordination()
            .notify_space(
                daemon,
                &workspace.name,
                &workspace.id,
                crate::notifications::AlertKind::HookFailed,
                &message,
            )
            .await;
        Some(message)
    }

    /// The hook sees the worktree intact. A failing hook retains the
    /// workspace.
    async fn run_pre_remove_hook(&self, daemon: &dyn Daemon, workspace: &Workspace) -> Result<()> {
        let Some(command) = self
            .removal_hook(daemon, workspace, HookKind::PreRemove)
            .await?
        else {
            return Ok(());
        };
        let settings = daemon.workspace_settings(workspace).await?;
        let repository = workspace.repository();
        let configured_env =
            (self.hook_environment(daemon, workspace, Some(repository), &settings)).await?;
        hooks::run_detached(
            Hook::PreRemove,
            workspace,
            &command,
            &self.paths,
            &configured_env,
        )
        .await
    }

    /// Committing the removal releases the leases. The hook is resolved only
    /// for leases to release, since resolving fails while the worktree's
    /// config file is rejected.
    async fn run_resource_release_hooks(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<()> {
        let leases = daemon
            .list_resources(Some(&SpaceSelector::id(&workspace.id)))
            .await?;
        if leases.is_empty() {
            return Ok(());
        }
        let release_hook = self
            .workspace_hook(daemon, workspace, HookKind::PreResourceRelease)
            .await?;
        for lease in leases {
            daemon
                .run_resource_release_hook(workspace, &lease, release_hook.as_deref())
                .await?;
        }
        Ok(())
    }

    /// A failed deletion may have deleted some files, so its checkpoint
    /// stays. The next attempt takes another, and both expire as usual.
    async fn delete_checkpointed_worktree(
        &self,
        daemon: &dyn Daemon,
        repo: &Path,
        workspace: &Workspace,
        removal: Removal<'_>,
        check: &Inspection,
    ) -> Result<WorktreeRemoval> {
        let record = self.save_restore_record(daemon, workspace, removal).await?;
        ensure_checkpoint_keeps_files(check, removal, record.as_ref())?;
        let built_on = self.branch_built_on(workspace).await?;
        let (files, branch) = removal_options(check, removal, built_on);
        let registration = self.existing_registration(workspace, repo).await?;
        self.remove_worktree(daemon, repo, workspace, files, branch, registration)
            .await
    }

    /// Disowned, or listed once the identity of its directory is recorded,
    /// which proves what Git leaves there should the deletion stop partway.
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

    /// Checkpoints HEAD and unsaved files. A disposable workspace keeps
    /// nothing to restore.
    async fn save_restore_record(
        &self,
        daemon: &dyn Daemon,
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
        self.save_removal_record(daemon, workspace, removal.cause(), carried_on)
            .await
            .map(Some)
    }

    /// Releases live resources, containers first, just before removing the
    /// worktree. Failed cleanup keeps their ownership records so removal can
    /// be retried. Git cannot tell which branch a worktree it no longer
    /// lists was on, so the recorded one stays.
    async fn remove_worktree(
        &self,
        daemon: &dyn Daemon,
        repo: &Path,
        workspace: &Workspace,
        files: FileRemoval,
        branch: BranchRemoval,
        registration: Registration,
    ) -> Result<WorktreeRemoval> {
        daemon.remove_containers(workspace).await?;
        daemon.remove_simulators(&workspace.id).await?;
        match registration {
            Registration::Listed => {
                daemon.destroy_seed_outputs(workspace).await;
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

    /// Only for a removal that [skips stalled](Removal::skips_stalled)
    /// spaces: refuses while the worktree is in a directory whose file
    /// access stalled. The sweep's own pass of that workspace rechecks it.
    pub fn ensure_worktree_answers(&self, workspace: &Workspace) -> Result<()> {
        ensure!(
            self.file_access.stalled(&workspace.path).is_none(),
            "file access stalled in the directory of {}, below it; retaining it",
            workspace.name
        );
        Ok(())
    }
}

/// True while the stopped removal's marker stays. A marker that cannot be
/// read counts too.
async fn stopped_removal_pending(workspace: &Workspace, state: WorkspaceState) -> bool {
    let Some(admin_dir) = workspace.checkout.git_dir() else {
        return false;
    };
    left_by_stopped_removal(workspace, state)
        && !matches!(interrupted::began(admin_dir).await, Ok(false))
}

/// Failed with that removal's error, which every other failure, and
/// readying the workspace, replaces.
fn left_by_stopped_removal(workspace: &Workspace, state: WorkspaceState) -> bool {
    state == WorkspaceState::Failed
        && (workspace.error.as_deref()).is_some_and(|error| error.starts_with(STOPPED_REMOVAL))
}

/// Refuses unless the checkpoint of `record` keeps the unsaved files that
/// `removal` discards or promises to keep restorable.
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

/// A branch goes when its commits are all preserved, except the default
/// branch, which an unresolved default branch could be, and a branch that
/// another workspace records building on, `built_on`. Files go by force
/// only once `removal` may discard unsaved work, or discards unsaved files a
/// checkpoint keeps. A workspace at a commit owns no branch to delete,
/// whichever its worktree is on. A disposable workspace's branch counts as
/// preserved while the worktree is still on it, and its files always go by
/// force.
pub fn removal_options(
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
