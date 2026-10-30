//! One removal path for explicit removal and automatic cleanup.
use super::{Manager, RemovalRules, executions::StopPolicy};
use crate::{
    daemon::{
        cleanup::{Rule, Snapshot},
        events::{self, EventCause, EventKind},
    },
    git::{
        self,
        default_branch::DefaultBranchLookup,
        worktrunk::{self, BranchRemoval, FileRemoval},
    },
    hooks::{self, Hook, HookKind},
    model::{CleanupReason, Workspace},
    removal::{
        self, BranchOutcome, IdleRestore, InspectionPolicy, NOTHING, RemovalCheck, RemovalResult,
        UnsavedWork,
    },
    state::WorkspaceState,
};
use anyhow::{Result, bail, ensure};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    path::PathBuf,
};

#[cfg(test)]
mod tests;

/// What keeps a workspace from every unattended removal. Sweeps report it
/// as a cleanup reason, and removal refuses with it at every stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention {
    Held,
    Question,
    /// A PR proposal waits for the user's approval.
    PrProposal,
    /// Workspaces split from this one remain.
    Children,
    /// An agent launch in the workspace waits in the agent queue or is starting.
    QueuedAgent,
}

impl From<Retention> for CleanupReason {
    fn from(retention: Retention) -> Self {
        match retention {
            Retention::Held => Self::Held,
            Retention::Question => Self::Question,
            Retention::PrProposal => Self::PrProposal,
            Retention::Children => Self::Children,
            Retention::QueuedAgent => Self::QueuedAgent,
        }
    }
}

/// Why removal refuses.
impl std::fmt::Display for Retention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Held => "workspace is held",
            Self::Question => "a question waits for the user",
            Self::PrProposal => "a PR proposal waits for approval",
            Self::Children => "workspaces split from it remain",
            Self::QueuedAgent => "an agent launch in it waits in the agent queue",
        })
    }
}

/// Who is removing the workspace, which decides how strict the checks are.
#[derive(Clone, Copy)]
enum Removal<'a> {
    /// Explicit merge completion stops tracked agents without waiting for idle.
    Merged { head: &'a str },
    /// An assignment finished; all commits must already be kept, unless
    /// another branch has them under other commits.
    Completed {
        head: &'a str,
        integrated: bool,
        cause: EventCause,
    },
    /// A user request; work no branch or remote keeps needs their consent.
    /// A workspace process's request also leaves the user's holds in place.
    Manual {
        unsaved: &'a UnsavedWork,
        inspection: InspectionPolicy,
        release_holds: bool,
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

impl<'a> Removal<'a> {
    fn cause(self) -> EventCause {
        match self {
            Self::Manual { .. } => EventCause::Manual,
            Self::Automatic { cause, .. } => cause,
            Self::Merged { .. } => EventCause::Pr,
            Self::Completed { cause, .. } => cause,
            Self::Deleted => EventCause::MissingDirectory,
        }
    }

    fn unsaved(self) -> &'a UnsavedWork {
        match self {
            Removal::Manual { unsaved, .. } => unsaved,
            Removal::Automatic { .. }
            | Removal::Deleted
            | Removal::Merged { .. }
            | Removal::Completed { .. } => &NOTHING,
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

    /// Automatic removal takes only unused workspaces, so a detached process
    /// running from one is activity that retains it; the others stop it.
    fn stops_detached_processes(self) -> bool {
        !matches!(self, Removal::Automatic { .. })
    }

    /// Merged cleanup removes unsaved files once a checkpoint keeps them.
    fn discards_archived_files(self) -> bool {
        matches!(
            self,
            Removal::Automatic {
                snapshot: Snapshot {
                    rule: Rule::Merged,
                    ..
                },
                ..
            }
        )
    }

    /// Unattended removals wait out the grace period, except to free disk
    /// space; a user's request does not.
    fn waits_for_grace(self) -> bool {
        match self {
            Removal::Merged { .. } | Removal::Completed { .. } => true,
            Removal::Automatic { cause, .. } => {
                !matches!(cause, EventCause::DiskSpace | EventCause::Manual)
            }
            Removal::Manual { .. } | Removal::Deleted => false,
        }
    }

    /// Unattended removals retain a workspace that is held, has an open
    /// question or has child workspaces, whenever it was recorded; a user's
    /// request overrides them.
    fn unattended(self) -> bool {
        matches!(
            self,
            Removal::Automatic { .. } | Removal::Merged { .. } | Removal::Completed { .. }
        )
    }

    /// Automatic removal needs a safe workspace; manual removal needs consent
    /// to delete unsaved work.
    fn verify(self, check: &RemovalCheck, stage: Stage) -> Result<()> {
        match (self, stage) {
            (Removal::Completed { integrated, .. }, _) => {
                ensure!(
                    !check.dirty,
                    "workspace has uncommitted or untracked work; retaining it"
                );
                ensure!(
                    integrated || check.commits_kept(),
                    "workspace has commits neither pushed nor on the local default branch or the branch it builds on; retaining it"
                );
            }
            (Removal::Merged { .. }, _) => ensure!(
                !check.dirty,
                "workspace has uncommitted or untracked work; retaining it"
            ),
            (Removal::Automatic { snapshot, .. }, Stage::Initial) => ensure!(
                snapshot.rule.allows(check),
                "workspace is no longer {}",
                snapshot.rule.describe()
            ),
            (Removal::Automatic { snapshot, .. }, _) => ensure!(
                snapshot.rule.allows(check),
                "workspace changed {}",
                stage.change()
            ),
            (Removal::Manual { unsaved, .. }, Stage::Initial) => ensure!(
                unsaved.uncovered(check).is_empty(),
                "removal would delete {}; confirm it or pass --yes",
                unsaved.uncovered(check).join(" and ")
            ),
            (Removal::Manual { unsaved, .. }, _) => ensure!(
                unsaved.uncovered(check).is_empty(),
                "workspace changed {}; removal would delete {} as well",
                stage.change(),
                unsaved.uncovered(check).join(" and ")
            ),
            (Removal::Deleted, _) => {}
        }
        Ok(())
    }
}

/// Recheck safety after operations that can change the worktree.
#[derive(Clone, Copy)]
enum Stage {
    Initial,
    AfterStop,
    /// Hooks run in the worktree.
    AfterHooks,
}

impl Stage {
    /// When a change that fails this stage's recheck happened.
    fn change(self) -> &'static str {
        match self {
            Stage::Initial => "before removal",
            Stage::AfterStop => "while stopping commands",
            Stage::AfterHooks => "during removal hooks",
        }
    }
}

/// What the worktree's state keeps from automatic cleanup, if anything.
fn worktree_cleanup_reason(check: &RemovalCheck) -> Option<CleanupReason> {
    if !check.workspace.path.is_dir() {
        Some(CleanupReason::MissingDirectory)
    } else if check.dirty {
        Some(CleanupReason::Dirty)
    } else if !check.commits_kept() {
        Some(CleanupReason::Unpushed {
            commits: check.unpushed_commits,
        })
    } else if check.running_commands > 0 {
        Some(CleanupReason::RunningCommands)
    } else if !check.processes.is_empty() {
        Some(CleanupReason::DirectoryProcesses {
            processes: check.processes.clone(),
        })
    } else {
        None
    }
}

/// The workspace's executions as cleanup sees them.
struct CleanupExecutions {
    /// Those other than the idle restores, which keep the workspace.
    running: usize,
    /// The restores of idle agents, with those a removal stopped.
    idle: Vec<IdleRestore>,
}

impl CleanupExecutions {
    /// Whether `execution` is one of the idle restores, whose recovery record
    /// the removal record keeps.
    fn idle_restore(&self, execution: &str) -> bool {
        self.idle
            .iter()
            .any(|restore| restore.execution == execution)
    }
}

/// A workspace reserved for removal: its record, whether its directory
/// existed, and the state the reservation replaced.
struct Reserved {
    workspace: Workspace,
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
    /// What removing the workspace at the user's request would do, with its
    /// changed files and whether removal archives them when
    /// `include_changes` is set and it has any.
    pub async fn check_manual_removal(
        &self,
        selector: &str,
        include_changes: bool,
    ) -> Result<RemovalCheck> {
        let mut check = self
            .check_removal(selector, InspectionPolicy::GitOnly)
            .await?;
        if include_changes && check.dirty {
            check.load_changed_files().await?;
            check.unsaved_files_archived = self.archives_unsaved_files(&check.workspace).await?;
        }
        Ok(check)
    }

    pub async fn check_removal(
        &self,
        selector: &str,
        policy: InspectionPolicy,
    ) -> Result<RemovalCheck> {
        let workspace = self.workspace(selector).await?;
        let executions = self.cleanup_executions(&workspace.id, &[]).await?;
        self.inspect_removal(workspace, policy, executions).await
    }

    async fn inspect_removal(
        &self,
        workspace: Workspace,
        policy: InspectionPolicy,
        executions: CleanupExecutions,
    ) -> Result<RemovalCheck> {
        if workspace.path.exists() {
            self.verify_worktree(&workspace).await?;
        }
        let repo = self.repository(&workspace.repository_id).await?;
        let default_branch =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached)
                .await
                .ok();
        removal::inspect(
            workspace,
            executions.running,
            executions.idle,
            policy,
            default_branch.as_deref(),
        )
        .await
    }

    /// The workspace's executions, telling the restores of idle agents, which
    /// keep it from no cleanup, from the others. `cancelled` are the idle
    /// restores a removal stopped, whose records and exiting wrappers it
    /// still disregards.
    async fn cleanup_executions(
        &self,
        id: &str,
        cancelled: &[IdleRestore],
    ) -> Result<CleanupExecutions> {
        let mut executions = CleanupExecutions {
            running: self.workspace_execution_count(id).await?,
            idle: cancelled.to_vec(),
        };
        let idle = self.idle_restores(id).await?;
        // Only the execution of an idle restore needs its details, and only
        // a waiting wrapper has one.
        let mut wrappers = HashMap::new();
        if !idle.is_empty() && executions.running > 0 {
            for execution in self.workspace_executions(id).await? {
                if idle.contains(&execution.id) {
                    executions.running -= 1;
                    wrappers.insert(execution.id, execution.wrapper);
                }
            }
        }
        executions.idle.extend(idle.into_iter().map(|execution| {
            IdleRestore {
                wrapper: wrappers
                    .remove(&execution)
                    .flatten()
                    .map(|wrapper| wrapper.pid),
                execution,
            }
        }));
        Ok(executions)
    }

    pub async fn remove_workspace(
        &self,
        selector: &str,
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
            },
        )
        .await
    }

    /// Remove the workspace as [`Self::remove_workspace`] does, refusing it
    /// while it has holds, which are the user's and host apps'.
    pub async fn remove_unheld_workspace(
        &self,
        selector: &str,
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
            },
        )
        .await
    }

    /// HEAD and activity of a cleanup candidate, cheap enough to compare on
    /// every sweep, or `None` when it is not a candidate.
    pub async fn cleanup_signals(&self, id: &str) -> Result<Option<u64>> {
        Ok(self
            .cleanup_candidate(id, &[])
            .await?
            .ok()
            .map(|(_, signals)| signals))
    }

    /// Everything automatic cleanup must see unchanged before removing the
    /// workspace, or `None` when it is not a cleanup candidate.
    pub async fn cleanup_snapshot(&self, id: &str) -> Result<Option<Snapshot>> {
        Ok(self.cleanup_candidacy(id).await?.ok())
    }

    /// The snapshot of a cleanup candidate, or why it is not one.
    pub async fn cleanup_candidacy(&self, id: &str) -> Result<Result<Snapshot, CleanupReason>> {
        self.idle_candidacy(id, &[]).await
    }

    /// The snapshot of a cleanup candidate whose `cancelled` restores a
    /// removal stopped, or why it is not one.
    async fn idle_candidacy(
        &self,
        id: &str,
        cancelled: &[IdleRestore],
    ) -> Result<Result<Snapshot, CleanupReason>> {
        let (path, signals) = match self.cleanup_candidate(id, cancelled).await? {
            Ok(candidate) => candidate,
            Err(reason) => return Ok(Err(reason)),
        };
        let tree = tokio::task::spawn_blocking(move || crate::daemon::cleanup::fingerprint(&path))
            .await??;
        Ok(Ok(Snapshot {
            rule: Rule::Idle,
            signals,
            tree,
        }))
    }

    /// Why automatic cleanup keeps the workspace, or its idle delay when
    /// nothing does; a failed inspection is reported as the reason.
    pub async fn cleanup_reason(&self, workspace: &Workspace) -> CleanupReason {
        let reason = async {
            if workspace.state != WorkspaceState::Ready {
                return Ok(CleanupReason::NotReady {
                    state: workspace.state,
                });
            }
            if let Err(reason) = self.cleanup_candidate(&workspace.id, &[]).await? {
                return Ok(reason);
            }
            let auto = self.workspace_settings(workspace).await?.cleanup.auto;
            Ok(if auto.enabled {
                CleanupReason::Idle {
                    minutes: auto.idle_minutes,
                }
            } else {
                CleanupReason::IdleDisabled
            })
        }
        .await;
        reason.unwrap_or_else(|error: anyhow::Error| CleanupReason::InspectionFailed {
            error: format!("{error:#}"),
        })
    }

    /// The path and cleanup signals of a candidate whose `cancelled`
    /// restores a removal stopped, or the first reason it is not one.
    async fn cleanup_candidate(
        &self,
        id: &str,
        cancelled: &[IdleRestore],
    ) -> Result<Result<(PathBuf, u64), CleanupReason>> {
        let executions = self.cleanup_executions(id, cancelled).await?;
        if let Some(reason) = self.recorded_cleanup_reason(id, &executions).await? {
            return Ok(Err(reason));
        }
        // Git state rules out most workspaces, so lsof only scans for safe ones.
        let mut check = self
            .inspect_removal(
                self.workspace(id).await?,
                InspectionPolicy::GitOnly,
                executions,
            )
            .await?;
        if let Some(reason) = worktree_cleanup_reason(&check) {
            return Ok(Err(reason));
        }
        check.load_directory_processes().await?;
        if let Some(reason) = worktree_cleanup_reason(&check) {
            return Ok(Err(reason));
        }
        let head = git::revision(&check.workspace.path, "HEAD", git::run).await?;
        let activity = self.usage.activity(id);
        Ok(Ok((
            check.workspace.path,
            crate::daemon::cleanup::signals(&head, activity),
        )))
    }

    /// The path and signals of a merged cleanup candidate, or `None` when
    /// it is not one: merged into the default branch and unused, with no Git
    /// operation under way, no workspace stacked on it or split from it, no
    /// hold or keep choice, and any unsaved files left over from merged work. Assignment
    /// state does not keep it. `cancelled` are the restores a removal stopped.
    async fn merged_candidate(
        &self,
        id: &str,
        cancelled: &[IdleRestore],
    ) -> Result<Option<(PathBuf, u64)>> {
        let executions = self.cleanup_executions(id, cancelled).await?;
        if self.keep_reason(id).await?.is_some()
            || self.usage_reason(id, &executions).await?.is_some()
        {
            return Ok(None);
        }
        let mut check = self
            .inspect_removal(
                self.workspace(id).await?,
                InspectionPolicy::GitOnly,
                executions,
            )
            .await?;
        if !check.workspace.path.is_dir()
            || !Rule::Merged.allows(&check)
            || git::status::operation_in_progress(&check.workspace.path).await?
            || (check.dirty && !self.unsaved_files_are_leftovers(&check.workspace).await?)
        {
            return Ok(None);
        }
        check.load_directory_processes().await?;
        if !Rule::Merged.allows(&check) {
            return Ok(None);
        }
        let head = git::revision(&check.workspace.path, "HEAD", git::run).await?;
        let activity = self.usage.activity(id);
        Ok(Some((
            check.workspace.path,
            crate::daemon::cleanup::signals(&head, activity),
        )))
    }

    /// The full snapshot of a merged cleanup candidate.
    pub async fn merged_snapshot(&self, id: &str) -> Result<Option<Snapshot>> {
        self.merged_snapshot_after(id, &[]).await
    }

    async fn merged_snapshot_after(
        &self,
        id: &str,
        cancelled: &[IdleRestore],
    ) -> Result<Option<Snapshot>> {
        let Some((path, signals)) = self.merged_candidate(id, cancelled).await? else {
            return Ok(None);
        };
        let tree = tokio::task::spawn_blocking(move || crate::daemon::cleanup::fingerprint(&path))
            .await??;
        Ok(Some(Snapshot {
            rule: Rule::Merged,
            signals,
            tree,
        }))
    }

    /// The signals an automatic removal compares after stopping commands
    /// and the `cancelled` restores, under the rule that made the workspace a
    /// candidate.
    async fn automatic_signals(
        &self,
        id: &str,
        rule: Rule,
        cancelled: &[IdleRestore],
    ) -> Result<Option<u64>> {
        Ok(match rule {
            Rule::Idle => self
                .cleanup_candidate(id, cancelled)
                .await?
                .ok()
                .map(|(_, signals)| signals),
            Rule::Merged => self
                .merged_candidate(id, cancelled)
                .await?
                .map(|(_, signals)| signals),
        })
    }

    async fn automatic_snapshot(
        &self,
        id: &str,
        rule: Rule,
        cancelled: &[IdleRestore],
    ) -> Result<Option<Snapshot>> {
        match rule {
            Rule::Idle => Ok(self.idle_candidacy(id, cancelled).await?.ok()),
            Rule::Merged => self.merged_snapshot_after(id, cancelled).await,
        }
    }

    /// What daemon records keep the workspace: holds, open questions and
    /// assignment state first, since their own cleanup paths decide removal,
    /// then stopped work besides the idle restores of `executions`, then
    /// commands and leases.
    async fn recorded_cleanup_reason(
        &self,
        id: &str,
        executions: &CleanupExecutions,
    ) -> Result<Option<CleanupReason>> {
        if let Some(retention) = self.keep_reason(id).await? {
            return Ok(Some(retention.into()));
        }
        if let Some(reason) = self.assignment_reason(id).await? {
            return Ok(Some(reason));
        }
        if crate::execution::recovery::pending_except(&self.paths, id, |execution| {
            executions.idle_restore(execution)
        })? {
            return Ok(Some(CleanupReason::Recovering));
        }
        self.usage_reason(id, executions).await
    }

    /// What keeps the workspace from every unattended removal, completion
    /// and PR cleanup included, read fresh on each call.
    pub(crate) async fn keep_reason(&self, id: &str) -> Result<Option<Retention>> {
        if self.has_holds(id).await? {
            return Ok(Some(Retention::Held));
        }
        if self.has_open_questions(id).await? {
            return Ok(Some(Retention::Question));
        }
        if self.has_pending_pull_proposal(id).await? {
            return Ok(Some(Retention::PrProposal));
        }
        if !self.workspace(id).await?.child_workspaces.is_empty() {
            return Ok(Some(Retention::Children));
        }
        Ok(self
            .has_queued_launch(id)
            .await?
            .then_some(Retention::QueuedAgent))
    }

    /// Assignment state whose own cleanup path decides removal.
    async fn assignment_reason(&self, id: &str) -> Result<Option<CleanupReason>> {
        if self.pr_registration(id).await?.is_some() {
            return Ok(Some(CleanupReason::PrWatch));
        }
        if let Some(completion) = self.completion(id).await? {
            return Ok(Some(CleanupReason::Completed {
                integrated: completion.integrated,
            }));
        }
        if self.issue_registration(id).await?.is_some() {
            return Ok(Some(CleanupReason::Issue));
        }
        Ok(None)
    }

    /// Active commands and leases; saved stopped work and idle restores do
    /// not imply use.
    async fn usage_reason(
        &self,
        id: &str,
        executions: &CleanupExecutions,
    ) -> Result<Option<CleanupReason>> {
        if executions.running > 0 {
            return Ok(Some(CleanupReason::RunningCommands));
        }
        if !self.list_resources(Some(id)).await?.is_empty() {
            return Ok(Some(CleanupReason::Permits));
        }
        if self
            .list_simulators(Some(id))
            .await?
            .iter()
            .any(|s| s.workspace_id.is_some())
        {
            return Ok(Some(CleanupReason::Simulators));
        }
        Ok(None)
    }

    /// `Ok(false)` while the grace period runs, as for every unattended removal.
    pub async fn remove_idle(&self, selector: &str, snapshot: Snapshot) -> Result<bool> {
        self.remove_unused(selector, snapshot, EventCause::Idle)
            .await
    }

    pub async fn remove_merged_idle(&self, selector: &str, snapshot: Snapshot) -> Result<bool> {
        self.remove_unused(selector, snapshot, EventCause::Merged)
            .await
    }

    /// Remove an idle cleanup candidate without waiting for its idle delay.
    pub async fn remove_for_disk_space(&self, selector: &str, snapshot: Snapshot) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::DiskSpace)
            .await
            .map(|_| ())
    }

    /// Remove an idle cleanup candidate on the user's request, without
    /// waiting for its idle delay.
    pub async fn remove_on_request(&self, selector: &str, snapshot: Snapshot) -> Result<()> {
        self.remove_unused(selector, snapshot, EventCause::Manual)
            .await
            .map(|_| ())
    }

    async fn remove_unused(
        &self,
        selector: &str,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        let (workspace, _guard) = self.lock_pr_gate(selector).await?;
        self.remove_after_grace(&workspace.id, Removal::Automatic { snapshot, cause })
            .await
    }

    pub async fn remove_merged(&self, selector: &str, head: &str) -> Result<bool> {
        self.remove_after_grace(selector, Removal::Merged { head })
            .await
    }

    pub(super) async fn remove_completed(
        &self,
        selector: &str,
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

    /// Remove the workspace unless its grace period is still running.
    async fn remove_after_grace(&self, selector: &str, removal: Removal<'_>) -> Result<bool> {
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
        self.recheck_removal(workspace, removal, Stage::Initial, &[])
            .await?;
        let grace = self.workspace_settings(workspace).await?.cleanup.grace();
        let cleanup_at = self.claim_grace(&workspace.id, grace).await?;
        Ok(crate::time::unix_seconds() < cleanup_at)
    }

    /// Forget a workspace whose directory was deleted outside Shoal.
    pub async fn remove_deleted(&self, selector: &str) -> Result<RemovalResult> {
        self.remove(selector, Removal::Deleted).await
    }

    async fn remove(&self, selector: &str, removal: Removal<'_>) -> Result<RemovalResult> {
        let Reserved {
            workspace,
            present,
            replaced: reserved_state,
        } = self.reserve_removal(selector, removal).await?;
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
    async fn reserve_removal(&self, selector: &str, removal: Removal<'_>) -> Result<Reserved> {
        let manual = matches!(removal, Removal::Manual { .. });
        let mut workspace = self.workspace(selector).await?;
        let _created = if manual && workspace.state == WorkspaceState::Preparing {
            let created = self.gates.lock_git(&workspace.repository_id).await;
            workspace = self.workspace(&workspace.id).await?;
            Some(created)
        } else {
            None
        };
        let present = workspace.path.try_exists()?;
        let rules = RemovalRules {
            allow_pending_rename: !present,
            awaiting_setup: manual && workspace.state == WorkspaceState::Preparing,
            refuse_holds: matches!(
                removal,
                Removal::Manual {
                    release_holds: false,
                    ..
                }
            ),
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

    async fn prepare_post_remove_hook(
        &self,
        workspace: &Workspace,
    ) -> Result<Option<PostRemoveHook>> {
        match self.workspace_hook(workspace, HookKind::PostRemove).await? {
            Some(command) => Ok(Some(PostRemoveHook {
                command,
                checkout: self.repository(&workspace.repository_id).await?.path,
                env: self.removed_workspace_environment(workspace).await?,
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
                crate::daemon::workspace_files::forget_removed(&tx, &id)?;
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
        self.close_workspace_sessions(workspace).await;
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
            if let Err(error) = self.stop_workspace_processes(&workspace.id).await {
                crate::daemon::log!(
                    "processes left by the post-remove hook of {} not stopped: {error:#}",
                    workspace.name
                );
            }
        }
        outcome
    }

    /// Session logs are run data owned by the record just deleted.
    fn delete_session_logs(&self, id: &str) {
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
        workspace: &crate::model::Workspace,
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
            &super::StopRequest::restorable_removal(),
        )
        .await?;
        self.recheck_removal(workspace, removal, Stage::AfterStop, &cancelled)
            .await?;
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
        // Processes can write files as they stop, which the final inspection
        // and the checkpoint must include.
        self.stop_workspace_processes(&workspace.id).await?;
        if removal.stops_detached_processes() {
            self.stop_detached_directory_processes(workspace).await?;
        }
        let check = self
            .recheck_removal(workspace, removal, Stage::AfterHooks, &cancelled)
            .await?;
        // The final inspection decides what goes.
        let (files, branch) = worktrunk_removal_options(&check, removal.unsaved());
        let record = self.save_removal_record(workspace, removal.cause()).await?;
        let removed = async {
            if check.dirty
                && (removal.discards_archived_files()
                    || removal.unsaved().promises_restorable_files())
            {
                ensure!(
                    !record.checkpoint.files_omitted,
                    "a checkpoint cannot keep the unsaved files; retaining the workspace"
                );
            }
            let files = if removal.discards_archived_files() && check.dirty {
                FileRemoval::Force
            } else {
                files
            };
            self.remove_worktree(&repo.path, workspace, files, branch, true)
                .await
        }
        .await;
        // A failed removal may have deleted some files, so its checkpoint
        // stays; the next attempt takes another, and both expire as usual.
        removed
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
    ) -> Result<RemovalCheck> {
        let executions = self.cleanup_executions(&workspace.id, cancelled).await?;
        let check = self
            .inspect_removal(
                self.workspace(&workspace.id).await?,
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
                !self.workspace_has_executions(&workspace.id).await?,
                "commands are recorded; stop them with shoal stop or shoal rm"
            );
        }
        self.stop_executions(
            &workspace.id,
            removal.stop_policy(),
            &super::StopRequest::removal(),
        )
        .await?;
        self.stop_workspace_processes(&workspace.id).await?;
        // Prune only this owned registration, through Worktrunk, and retain
        // its branch because the contents cannot be inspected.
        let registered = self.is_registered_worktree(workspace).await?;
        self.remove_worktree(
            &repo.path,
            workspace,
            FileRemoval::Force,
            BranchRemoval::Keep,
            registered,
        )
        .await
    }

    /// Release live resources immediately before removing the worktree. Failed
    /// cleanup retains their ownership records so removal can be retried.
    async fn remove_worktree(
        &self,
        repo: &std::path::Path,
        workspace: &crate::model::Workspace,
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
                branch: Some(workspace.branch.clone()),
                branch_outcome: BranchOutcome::Retained,
                hook_error: None,
            })
        }
    }
}

/// Worktrunk's options for the inspected worktree. A branch goes when its
/// commits are all preserved, except the default branch, which an
/// unresolved default branch could be; files go by force only once the
/// removal may discard unsaved work.
fn worktrunk_removal_options(
    check: &RemovalCheck,
    unsaved: &UnsavedWork,
) -> (FileRemoval, BranchRemoval) {
    let branch = if check.branch_preserved()
        && check
            .default_branch
            .as_deref()
            .is_some_and(|default| check.branch.as_deref() != Some(default))
    {
        BranchRemoval::Delete
    } else {
        BranchRemoval::Keep
    };
    let files = if *unsaved == NOTHING {
        FileRemoval::CleanOnly
    } else {
        FileRemoval::Force
    };
    (files, branch)
}
