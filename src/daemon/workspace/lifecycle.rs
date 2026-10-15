//! One removal path for explicit removal and automatic cleanup.
use super::{Manager, executions::StopPolicy};
use crate::{
    daemon::{
        cleanup::Snapshot,
        events::{self, EventCause, EventKind},
    },
    git::{
        default_branch::DefaultBranchLookup,
        worktrunk::{self, BranchRemoval, FileRemoval},
    },
    hooks::{self, Hook, HookKind},
    model::Workspace,
    removal::{self, BranchChoice, BranchOutcome, InspectionPolicy, RemovalCheck, RemovalResult},
    state::WorkspaceState,
};
use anyhow::{Result, bail, ensure};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

#[cfg(test)]
mod tests;

/// Who is removing the workspace, which decides how strict the checks are.
#[derive(Clone, Copy)]
enum Removal<'a> {
    /// Explicit merge completion stops tracked agents without waiting for idle.
    Merged { head: &'a str },
    /// An assignment finished; all commits must already be preserved.
    Completed { head: &'a str, cause: EventCause },
    /// A user request; dirty or differing work needs an explicit branch choice.
    Manual {
        choice: BranchChoice,
        inspection: InspectionPolicy,
    },
    /// Idle or disk-space cleanup; only safe, unchanged workspaces may go.
    Automatic {
        snapshot: Snapshot,
        cause: EventCause,
    },
    /// The directory was deleted outside Shoal; forget the workspace and
    /// release what it owned, retaining its branch.
    Deleted,
}

impl Removal<'_> {
    fn cause(self) -> EventCause {
        match self {
            Self::Manual { .. } => EventCause::Manual,
            Self::Automatic { cause, .. } => cause,
            Self::Merged { .. } => EventCause::Pr,
            Self::Completed { cause, .. } => cause,
            Self::Deleted => EventCause::MissingDirectory,
        }
    }

    fn choice(self) -> BranchChoice {
        match self {
            Removal::Manual { choice, .. } => choice,
            Removal::Automatic { .. }
            | Removal::Deleted
            | Removal::Merged { .. }
            | Removal::Completed { .. } => BranchChoice::Auto,
        }
    }

    fn inspection(self) -> InspectionPolicy {
        match self {
            Removal::Manual { inspection, .. } => inspection,
            Removal::Automatic { .. }
            | Removal::Deleted
            | Removal::Merged { .. }
            | Removal::Completed { .. } => InspectionPolicy::IncludeDirectoryProcesses,
        }
    }

    fn stop_policy(self) -> StopPolicy {
        match self {
            Removal::Manual { .. } | Removal::Merged { .. } | Removal::Completed { .. } => {
                StopPolicy::ForRemoval
            }
            Removal::Automatic { .. } | Removal::Deleted => StopPolicy::RequireCompleteProof,
        }
    }

    /// Unattended removals retain a held workspace, whenever the hold was
    /// taken; a user's request overrides holds.
    fn respects_holds(self) -> bool {
        matches!(
            self,
            Removal::Automatic { .. } | Removal::Merged { .. } | Removal::Completed { .. }
        )
    }

    /// Automatic removal needs a safe workspace; manual removal with `Auto`
    /// needs a workspace that does not require a branch choice.
    fn verify(self, check: &RemovalCheck, stage: Stage) -> Result<()> {
        match (self, stage) {
            (Removal::Completed { .. }, _) => {
                ensure!(
                    !check.dirty,
                    "workspace has uncommitted or untracked work; retaining it"
                );
                ensure!(
                    check.unpushed_commits == 0,
                    "workspace has commits neither pushed nor on the local default branch; retaining it"
                );
            }
            (Removal::Merged { .. }, _) => ensure!(
                !check.dirty,
                "workspace has uncommitted or untracked work; retaining it"
            ),
            (Removal::Automatic { .. }, Stage::Initial) => ensure!(
                check.safe(),
                "workspace is no longer idle, clean and fully pushed"
            ),
            (Removal::Automatic { .. }, _) => {
                ensure!(check.safe(), "workspace changed {}", stage.change())
            }
            (
                Removal::Manual {
                    choice: BranchChoice::Auto,
                    ..
                },
                Stage::Initial,
            ) => ensure!(
                !check.needs_choice(),
                "removal requires a branch choice: {}; use --yes with --keep-branch or --delete-branch",
                check.warnings().join("; ")
            ),
            (
                Removal::Manual {
                    choice: BranchChoice::Auto,
                    ..
                },
                _,
            ) => ensure!(
                !check.needs_choice(),
                "workspace changed {}; choose whether to keep or delete the branch",
                stage.change()
            ),
            (Removal::Manual { .. } | Removal::Deleted, _) => {}
        }
        Ok(())
    }
}

/// Recheck safety after operations that can change the worktree.
#[derive(Clone, Copy)]
enum Stage<'a> {
    Initial,
    AfterStop,
    /// Hooks run in the worktree; `head` is its HEAD before they ran.
    AfterHooks {
        head: &'a str,
    },
}

impl Stage<'_> {
    /// When a change that fails this stage's recheck happened.
    fn change(self) -> &'static str {
        match self {
            Stage::Initial => "before removal",
            Stage::AfterStop => "while stopping commands",
            Stage::AfterHooks { .. } => "during removal hooks",
        }
    }
}

/// A post-remove hook resolved while the workspace record still exists.
struct PostRemoveHook {
    command: PathBuf,
    checkout: PathBuf,
    env: BTreeMap<String, OsString>,
}

impl Manager {
    pub async fn check_removal(
        &self,
        selector: &str,
        policy: InspectionPolicy,
    ) -> Result<RemovalCheck> {
        let inspection = self.inspect_workspace(selector).await?;
        if inspection.workspace.path.exists() {
            self.verify_worktree(&inspection.workspace).await?;
        }
        let repo = self.repository(&inspection.workspace.repository_id).await?;
        let default_branch =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached)
                .await
                .ok();
        removal::inspect(
            inspection.workspace,
            inspection.executions.len(),
            policy,
            default_branch.as_deref(),
        )
        .await
    }

    pub async fn remove_workspace(
        &self,
        selector: &str,
        choice: BranchChoice,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        self.remove(selector, Removal::Manual { choice, inspection })
            .await
    }

    /// HEAD and activity of a cleanup candidate, cheap enough to compare on
    /// every sweep, or `None` when it is not a candidate.
    pub async fn cleanup_signals(&self, id: &str) -> Result<Option<u64>> {
        Ok(self
            .cleanup_candidate(id)
            .await?
            .map(|(_, signals)| signals))
    }

    /// Everything automatic cleanup must see unchanged before removing the
    /// workspace, or `None` when it is not a cleanup candidate.
    pub async fn cleanup_snapshot(&self, id: &str) -> Result<Option<Snapshot>> {
        let Some((path, signals)) = self.cleanup_candidate(id).await? else {
            return Ok(None);
        };
        let tree = tokio::task::spawn_blocking(move || crate::daemon::cleanup::fingerprint(&path))
            .await??;
        Ok(Some(Snapshot { signals, tree }))
    }

    async fn cleanup_candidate(&self, id: &str) -> Result<Option<(PathBuf, u64)>> {
        if crate::execution::recovery::pending(&self.paths, id)?
            || self.workspace_has_executions(id).await?
        {
            return Ok(None);
        }
        if self.completion(id).await?.is_some()
            || self.pr_registration(id).await?.is_some()
            || self.issue_registration(id).await?.is_some()
        {
            return Ok(None);
        }
        if self.has_holds(id).await? || !self.list_resources(Some(id)).await?.is_empty() {
            return Ok(None);
        }
        if self
            .list_simulators(Some(id))
            .await?
            .iter()
            .any(|s| s.workspace_id.is_some())
        {
            return Ok(None);
        }
        // Git state rules out most workspaces, so lsof only scans for safe ones.
        let mut check = self.check_removal(id, InspectionPolicy::GitOnly).await?;
        if !check.safe() || !check.workspace.path.is_dir() {
            return Ok(None);
        }
        check.load_directory_processes().await?;
        if !check.safe() {
            return Ok(None);
        }
        let head = crate::git::run(&check.workspace.path, &["rev-parse", "HEAD"]).await?;
        let activity = self.activity(id).await;
        Ok(Some((
            check.workspace.path,
            crate::daemon::cleanup::signals(&head, activity),
        )))
    }

    pub async fn remove_idle(&self, selector: &str, snapshot: Snapshot) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::Idle)
            .await
    }

    /// Remove an idle cleanup candidate without waiting for its idle delay.
    pub async fn remove_for_disk_space(&self, selector: &str, snapshot: Snapshot) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::DiskSpace)
            .await
    }

    /// Remove an idle cleanup candidate on the user's request, without
    /// waiting for its idle delay.
    pub async fn remove_on_request(&self, selector: &str, snapshot: Snapshot) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::Manual)
            .await
    }

    async fn remove_unused(
        &self,
        selector: &str,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<()> {
        let _guard = self.pr_gate.lock().await;
        self.remove(selector, Removal::Automatic { snapshot, cause })
            .await
            .map(|_| ())
    }

    pub async fn remove_merged(&self, selector: &str, head: &str) -> Result<()> {
        self.remove(selector, Removal::Merged { head })
            .await
            .map(|_| ())
    }

    pub(super) async fn remove_completed(
        &self,
        selector: &str,
        head: &str,
        cause: EventCause,
    ) -> Result<()> {
        self.remove(selector, Removal::Completed { head, cause })
            .await
            .map(|_| ())
    }

    /// Forget a workspace whose directory was deleted outside Shoal.
    pub async fn remove_deleted(&self, selector: &str) -> Result<RemovalResult> {
        self.remove(selector, Removal::Deleted).await
    }

    async fn remove(&self, selector: &str, removal: Removal<'_>) -> Result<RemovalResult> {
        let workspace = self.workspace(selector).await?;
        let present = workspace.path.try_exists()?;
        self.reserve_lifecycle_for_removal(&workspace.id, !present)
            .await?;
        // Checked again after the reservation, so a worktree restored meanwhile
        // cannot be deleted by an unattended removal.
        let result = async {
            let present = workspace.path.try_exists()?;
            let post_remove = if present {
                self.ensure_no_pending_rename(&workspace.id).await?;
                self.prepare_post_remove_hook(&workspace).await?
            } else {
                None
            };
            let outcome = match removal {
                Removal::Deleted if present => bail!("workspace directory exists"),
                _ if present => self.remove_present_worktree(&workspace, removal).await?,
                _ => self.remove_missing_worktree(&workspace, removal).await?,
            };
            self.remove_simulators(&workspace.id).await?;
            self.commit_workspace_removal(
                &workspace.id,
                if present {
                    removal.cause()
                } else {
                    EventCause::MissingDirectory
                },
            )
            .await?;
            Ok((outcome, post_remove))
        }
        .await;
        match result {
            Ok((outcome, post_remove)) => Ok(self
                .complete_workspace_removal(&workspace, outcome, post_remove)
                .await),
            Err(error) => {
                // A deleted directory can no longer be ready.
                let state = match removal {
                    Removal::Deleted if !present => WorkspaceState::Failed,
                    _ => workspace.state,
                };
                self.retain_workspace(&workspace.id, state, removal.cause(), &error)
                    .await?;
                Err(error)
            }
        }
    }

    async fn prepare_post_remove_hook(
        &self,
        workspace: &Workspace,
    ) -> Result<Option<PostRemoveHook>> {
        match self.workspace_hook(workspace, HookKind::PostRemove).await? {
            Some(command) => Ok(Some(PostRemoveHook {
                command,
                checkout: self.repository(&workspace.repository_id).await?.path,
                env: self.configured_workspace_environment(workspace).await?,
            })),
            None => Ok(None),
        }
    }

    /// Release ownership and cascading resource leases in one transaction.
    async fn commit_workspace_removal(&self, id: &str, cause: EventCause) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                events::record(&tx, &id, EventKind::Removed, cause, None)?;
                tx.execute("DELETE FROM executions WHERE workspace_id=?1", [&id])?;
                tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
                tx.commit()?;
                Ok(())
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
        self.forget_workspace(&workspace.id).await;
        // Session logs are run data owned by the record just deleted.
        let _ = std::fs::remove_dir_all(self.paths.workspace_state(&workspace.id));
        if let Some(hook) = post_remove
            && let Err(error) = hooks::run_detached(
                Hook::PostRemove(&hook.checkout),
                workspace,
                &hook.command,
                &self.paths,
                &hook.env,
            )
            .await
        {
            let message = format!("workspace removed; {error:#}");
            self.notify(
                Some(&workspace.name),
                crate::daemon::notifications::NotificationKind::HookFailed,
                &message,
            )
            .await;
            outcome.hook_error = Some(message);
        }
        outcome
    }

    async fn remove_present_worktree(
        &self,
        workspace: &crate::model::Workspace,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let repo = self.repository(&workspace.repository_id).await?;
        self.recheck_removal(workspace, removal, Stage::Initial)
            .await?;
        self.stop_executions(
            &workspace.id,
            removal.stop_policy(),
            &super::StopRequest::removal(),
        )
        .await?;
        let check = self
            .recheck_removal(workspace, removal, Stage::AfterStop)
            .await?;
        // Validate explicit choices before hooks, then derive the actual
        // adapter options again from the final inspection.
        worktrunk_removal_options(&repo.path, &check, removal.choice()).await?;
        let head_before_hooks = crate::git::run(&workspace.path, &["rev-parse", "HEAD"]).await?;
        // The hook sees the worktree intact; a failing hook retains it.
        if let Some(command) = self.workspace_hook(workspace, HookKind::PreRemove).await? {
            let configured_env = self.configured_workspace_environment(workspace).await?;
            hooks::run_detached(
                Hook::PreRemove,
                workspace,
                &command,
                &self.paths,
                &configured_env,
            )
            .await?;
        }
        let release_hook = self
            .workspace_hook(workspace, HookKind::PreResourceRelease)
            .await?;
        for lease in self.list_resources(Some(&workspace.id)).await? {
            self.run_resource_release_hook(workspace, &lease, release_hook.as_deref())
                .await?;
        }
        let check = self
            .recheck_removal(
                workspace,
                removal,
                Stage::AfterHooks {
                    head: &head_before_hooks,
                },
            )
            .await?;
        let (files, branch) =
            worktrunk_removal_options(&repo.path, &check, removal.choice()).await?;
        // Live resources are removed before the directory; failed cleanup
        // retains their ownership records so removal can be retried.
        self.remove_simulators(&workspace.id).await?;
        worktrunk::remove(
            &repo.path,
            &self.paths.worktrunk_config(),
            &workspace.path,
            files,
            branch,
        )
        .await
    }

    /// Inspect the workspace again and apply every check `removal` needs at
    /// `stage`, returning the inspection removal proceeds with.
    async fn recheck_removal(
        &self,
        workspace: &Workspace,
        removal: Removal<'_>,
        stage: Stage<'_>,
    ) -> Result<RemovalCheck> {
        let check = self
            .check_removal(&workspace.id, removal.inspection())
            .await?;
        if removal.respects_holds() {
            ensure!(
                check.workspace.holds.is_empty(),
                "workspace is held; retaining it"
            );
        }
        removal.verify(&check, stage)?;
        match (removal, stage) {
            (Removal::Merged { head } | Removal::Completed { head, .. }, _) => ensure!(
                crate::forge::pr::current_head(workspace).await? == head,
                "HEAD changed {}",
                stage.change()
            ),
            // Later stages compare against the sweep's snapshot, so they also
            // cover changes made before removal started.
            (Removal::Automatic { .. }, Stage::Initial) => {}
            (Removal::Automatic { snapshot, .. }, Stage::AfterStop) => ensure!(
                self.cleanup_signals(&workspace.id).await? == Some(snapshot.signals),
                "workspace changed {}",
                stage.change()
            ),
            // The tree is compared only after the hooks: change times only
            // move forward, so that also covers changes made before them.
            (Removal::Automatic { snapshot, .. }, Stage::AfterHooks { .. }) => ensure!(
                self.cleanup_snapshot(&workspace.id).await? == Some(snapshot),
                "workspace changed {}",
                stage.change()
            ),
            (
                Removal::Manual {
                    choice: BranchChoice::Auto,
                    ..
                },
                Stage::AfterHooks { head },
            ) => ensure!(
                crate::git::run(&workspace.path, &["rev-parse", "HEAD"]).await? == head,
                "HEAD changed {}; choose whether to keep or delete the branch",
                stage.change()
            ),
            (Removal::Manual { .. } | Removal::Deleted, _) => {}
        }
        Ok(check)
    }

    /// The directory is gone: a worktree that was deleted rather than moved is
    /// forgotten, and its branch is always retained.
    async fn remove_missing_worktree(
        &self,
        workspace: &crate::model::Workspace,
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
                self.inspect_workspace(&workspace.id)
                    .await?
                    .executions
                    .is_empty(),
                "commands are recorded; stop them with shoal stop or shoal rm"
            );
        }
        self.stop_executions(
            &workspace.id,
            removal.stop_policy(),
            &super::StopRequest::removal(),
        )
        .await?;
        self.remove_simulators(&workspace.id).await?;
        if self.is_registered_worktree(workspace).await? {
            // Prune only this owned registration, through Worktrunk, and
            // retain its branch because the contents cannot be inspected.
            worktrunk::remove(
                &repo.path,
                &self.paths.worktrunk_config(),
                &workspace.path,
                FileRemoval::Force,
                BranchRemoval::Keep,
            )
            .await
        } else {
            Ok(RemovalResult {
                removed: true,
                branch: Some(workspace.branch.clone()),
                branch_outcome: BranchOutcome::Retained,
                hook_error: None,
            })
        }
    }
}

async fn worktrunk_removal_options(
    checkout: &Path,
    check: &RemovalCheck,
    choice: BranchChoice,
) -> Result<(FileRemoval, BranchRemoval)> {
    let default_branch = crate::git::default_branch::resolve(checkout, DefaultBranchLookup::Cached)
        .await
        .ok();
    // An unresolved default branch could be any branch, so keep it.
    let branch = match choice {
        BranchChoice::Auto
            if check.can_delete_branch()
                && default_branch
                    .is_some_and(|default| check.branch.as_deref() != Some(&default)) =>
        {
            BranchRemoval::Delete
        }
        BranchChoice::Auto | BranchChoice::KeepBranch => BranchRemoval::Keep,
        BranchChoice::DeleteBranch => BranchRemoval::Delete,
    };
    let files = match choice {
        BranchChoice::Auto => FileRemoval::CleanOnly,
        BranchChoice::KeepBranch | BranchChoice::DeleteBranch => FileRemoval::Force,
    };
    ensure!(
        !matches!(choice, BranchChoice::KeepBranch)
            || check.branch.is_some()
            || check.unpushed_commits == 0,
        "detached HEAD has unpushed commits; create a branch before choosing to keep it"
    );
    Ok((files, branch))
}
