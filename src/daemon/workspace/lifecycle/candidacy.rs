//! Cleanup candidacy: the workspace as cleanup inspects it, what keeps it
//! from automatic cleanup, and the snapshot of a candidate.
use super::super::Manager;
use super::Retention;
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::cleanup::{Rule, Snapshot},
    fsutil::off_runtime,
    git::{self, default_branch::DefaultBranchLookup},
    model::{CleanupReason, Workspace},
    process,
    removal::{self, IdleRestore, Inspection, InspectionPolicy, RemovalCheck},
    state::WorkspaceState,
};
use anyhow::Result;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// What the worktree's state keeps from automatic cleanup, if anything.
async fn worktree_cleanup_reason(check: &RemovalCheck) -> Result<Option<CleanupReason>> {
    Ok(if !off_runtime::is_dir(&check.workspace.path).await? {
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
    })
}

/// Whether a process `users` finds uses the directory, besides the wrappers
/// waiting to restore the idle agents of `executions`.
async fn directory_in_use(
    path: &Path,
    executions: &CleanupExecutions,
    users: &process::DirectoryUsers,
) -> Result<bool> {
    let wrappers = removal::wrappers(&executions.idle);
    Ok(!users.in_directory_except(path, &wrappers).await?.is_empty())
}

/// The workspace's executions as cleanup sees them.
pub(super) struct CleanupExecutions {
    /// Those other than the idle restores, which keep the workspace.
    running: usize,
    /// The restores of idle agents, with those a removal stopped.
    idle: Vec<IdleRestore>,
}

impl CleanupExecutions {
    /// Whether `execution` is one of the idle restores, whose recovery record
    /// the removal record keeps.
    fn idle_restore(&self, execution: &ExecutionId) -> bool {
        self.idle
            .iter()
            .any(|restore| restore.execution == *execution)
    }
}

impl Manager {
    pub(super) async fn inspect_removal(
        &self,
        workspace: Workspace,
        policy: InspectionPolicy,
        executions: CleanupExecutions,
    ) -> Result<Inspection> {
        if off_runtime::exists(&workspace.path).await? {
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
    pub(super) async fn cleanup_executions(
        &self,
        id: &SpaceId,
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

    /// HEAD and activity of a workspace whose idle timer runs, or `None`
    /// when what Shoal records about it, or a process `users` finds in it,
    /// keeps it. Its Git state and files are left to the full inspections
    /// when the timer starts and when it expires, which fail or differ after
    /// any change to them.
    pub async fn idle_timer_signals(
        &self,
        id: &SpaceId,
        users: &process::DirectoryUsers,
    ) -> Result<Option<u64>> {
        let path = self.workspace_by_id(id).await?.path;
        self.check_access(&path, self.timer_signals(id, &path, users))
            .await
    }

    async fn timer_signals(
        &self,
        id: &SpaceId,
        path: &Path,
        users: &process::DirectoryUsers,
    ) -> Result<Option<u64>> {
        let executions = self.cleanup_executions(id, &[]).await?;
        if self
            .recorded_cleanup_reason(id, &executions)
            .await?
            .is_some()
            || !off_runtime::is_dir(path).await?
            || directory_in_use(path, &executions, users).await?
        {
            return Ok(None);
        }
        Ok(Some(self.current_signals(id, path).await?))
    }

    /// The HEAD and recorded activity of the workspace at `path`, hashed for
    /// comparison between sweeps.
    async fn current_signals(&self, id: &SpaceId, path: &Path) -> Result<u64> {
        let head = git::revision(path, "HEAD", git::run).await?;
        let activity = self.usage.activity(id);
        Ok(crate::daemon::cleanup::signals(&head, activity))
    }

    /// [`Self::swept_snapshot`], scanning the processes in the worktree.
    #[cfg(test)]
    pub(crate) async fn cleanup_snapshot(&self, id: &SpaceId) -> Result<Option<Snapshot>> {
        self.swept_snapshot(id, &process::DirectoryUsers::default())
            .await
    }

    /// Everything automatic cleanup must see unchanged before removing the
    /// workspace, or `None` when it is not a cleanup candidate, with the
    /// processes in the worktree read from `users`.
    pub async fn swept_snapshot(
        &self,
        id: &SpaceId,
        users: &process::DirectoryUsers,
    ) -> Result<Option<Snapshot>> {
        Ok(self.idle_candidacy(id, &[], users).await?.ok())
    }

    /// The snapshot of a cleanup candidate, or why it is not one.
    pub async fn cleanup_candidacy(&self, id: &SpaceId) -> Result<Result<Snapshot, CleanupReason>> {
        self.idle_candidacy(id, &[], &process::DirectoryUsers::default())
            .await
    }

    /// The snapshot of a cleanup candidate whose `cancelled` restores a
    /// removal stopped, or why it is not one.
    async fn idle_candidacy(
        &self,
        id: &SpaceId,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Result<Snapshot, CleanupReason>> {
        let (path, signals) = match self.checked_cleanup_candidate(id, cancelled, users).await? {
            Ok(candidate) => candidate,
            Err(reason) => return Ok(Err(reason)),
        };
        let tree = tokio::task::spawn_blocking(move || crate::daemon::cleanup::walk(&path))
            .await??
            .fingerprint;
        Ok(Ok(Snapshot {
            rule: Rule::Idle,
            signals,
            tree,
        }))
    }

    /// Why automatic cleanup keeps the workspace, or its idle delay when
    /// nothing does, inspected under [`Manager::check_access`]; a failed
    /// inspection is reported as the reason.
    pub async fn cleanup_reason(&self, workspace: &Workspace) -> CleanupReason {
        self.check_access(&workspace.path, self.inspect_cleanup(workspace))
            .await
            .unwrap_or_else(|error| CleanupReason::InspectionFailed {
                error: format!("{error:#}"),
            })
    }

    async fn inspect_cleanup(&self, workspace: &Workspace) -> Result<CleanupReason> {
        if workspace.state != WorkspaceState::Ready {
            return Ok(CleanupReason::NotReady {
                state: workspace.state,
            });
        }
        let users = process::DirectoryUsers::default();
        if let Err(reason) = self.cleanup_candidate(&workspace.id, &[], &users).await? {
            return Ok(reason);
        }
        let auto = self
            .workspace_settings(workspace)
            .await?
            .into_settings()
            .cleanup
            .auto;
        Ok(if auto.enabled {
            CleanupReason::Idle {
                minutes: auto.idle_minutes,
            }
        } else {
            CleanupReason::IdleDisabled
        })
    }

    /// [`Self::cleanup_candidate`], inspected under [`Manager::check_access`].
    async fn checked_cleanup_candidate(
        &self,
        id: &SpaceId,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Result<(PathBuf, u64), CleanupReason>> {
        let path = self.workspace_by_id(id).await?.path;
        self.check_access(&path, self.cleanup_candidate(id, cancelled, users))
            .await
    }

    /// The path and cleanup signals of a candidate whose `cancelled`
    /// restores a removal stopped, or the first reason it is not one; the
    /// processes in the worktree are read from `users`.
    async fn cleanup_candidate(
        &self,
        id: &SpaceId,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Result<(PathBuf, u64), CleanupReason>> {
        let executions = self.cleanup_executions(id, cancelled).await?;
        if let Some(reason) = self.recorded_cleanup_reason(id, &executions).await? {
            return Ok(Err(reason));
        }
        // Git state rules out most workspaces, so only safe ones read the
        // processes' working directories.
        let mut check = self
            .inspect_removal(
                self.workspace_by_id(id).await?,
                InspectionPolicy::GitOnly,
                executions,
            )
            .await?;
        if let Some(reason) = worktree_cleanup_reason(&check).await? {
            return Ok(Err(reason));
        }
        check.load_directory_processes(users).await?;
        if let Some(reason) = worktree_cleanup_reason(&check).await? {
            return Ok(Err(reason));
        }
        let signals = self.current_signals(id, &check.workspace.path).await?;
        Ok(Ok((check.check.workspace.path, signals)))
    }

    /// [`Self::merged_candidate`], inspected under [`Manager::check_access`].
    async fn checked_merged_candidate(
        &self,
        id: &SpaceId,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<(PathBuf, u64)>> {
        let path = self.workspace_by_id(id).await?.path;
        self.check_access(&path, self.merged_candidate(id, cancelled, users))
            .await
    }

    /// The path and signals of a merged cleanup candidate, or `None` when
    /// it is not one: merged into the default branch and unused, with no Git
    /// operation under way, no workspace stacked on it, no hold or keep
    /// choice, and any unsaved files left over from merged work. Assignment
    /// state does not keep it. `cancelled` are the restores a removal stopped,
    /// and the processes in the worktree are read from `users`.
    async fn merged_candidate(
        &self,
        id: &SpaceId,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<(PathBuf, u64)>> {
        let executions = self.cleanup_executions(id, cancelled).await?;
        if self.keep_reason(id).await?.is_some()
            || self.usage_reason(id, &executions).await?.is_some()
        {
            return Ok(None);
        }
        let mut check = self
            .inspect_removal(
                self.workspace_by_id(id).await?,
                InspectionPolicy::GitOnly,
                executions,
            )
            .await?;
        if !off_runtime::is_dir(&check.workspace.path).await?
            || !Rule::Merged.allows(&check)
            || git::status::operation_in_progress(&check.workspace.path).await?
            || (check.dirty && !self.unsaved_files_are_leftovers(&check.workspace).await?)
        {
            return Ok(None);
        }
        check.load_directory_processes(users).await?;
        if !Rule::Merged.allows(&check) {
            return Ok(None);
        }
        let signals = self.current_signals(id, &check.workspace.path).await?;
        Ok(Some((check.check.workspace.path, signals)))
    }

    /// The path and signals of a merged cleanup candidate, with the
    /// processes in the worktree read from `users`, before the walk of its
    /// files a full snapshot takes.
    pub async fn merged_signals(
        &self,
        id: &SpaceId,
        users: &process::DirectoryUsers,
    ) -> Result<Option<(PathBuf, u64)>> {
        self.checked_merged_candidate(id, &[], users).await
    }

    /// The full snapshot of a merged cleanup candidate.
    pub async fn merged_snapshot(&self, id: &SpaceId) -> Result<Option<Snapshot>> {
        self.merged_snapshot_after(id, &[], &process::DirectoryUsers::default())
            .await
    }

    async fn merged_snapshot_after(
        &self,
        id: &SpaceId,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<Snapshot>> {
        let Some((path, signals)) = self.checked_merged_candidate(id, cancelled, users).await?
        else {
            return Ok(None);
        };
        let tree = tokio::task::spawn_blocking(move || crate::daemon::cleanup::walk(&path))
            .await??
            .fingerprint;
        Ok(Some(Snapshot {
            rule: Rule::Merged,
            signals,
            tree,
        }))
    }

    /// The signals an automatic removal compares after stopping commands
    /// and the `cancelled` restores, under the rule that made the workspace a
    /// candidate, with the processes in the worktree scanned afresh.
    pub(super) async fn automatic_signals(
        &self,
        id: &SpaceId,
        rule: Rule,
        cancelled: &[IdleRestore],
    ) -> Result<Option<u64>> {
        let users = process::DirectoryUsers::default();
        Ok(match rule {
            Rule::Idle => self
                .checked_cleanup_candidate(id, cancelled, &users)
                .await?
                .ok()
                .map(|(_, signals)| signals),
            Rule::Merged => self
                .checked_merged_candidate(id, cancelled, &users)
                .await?
                .map(|(_, signals)| signals),
        })
    }

    pub(super) async fn automatic_snapshot(
        &self,
        id: &SpaceId,
        rule: Rule,
        cancelled: &[IdleRestore],
    ) -> Result<Option<Snapshot>> {
        let users = process::DirectoryUsers::default();
        match rule {
            Rule::Idle => Ok(self.idle_candidacy(id, cancelled, &users).await?.ok()),
            Rule::Merged => self.merged_snapshot_after(id, cancelled, &users).await,
        }
    }

    /// What daemon records keep the workspace: holds, open questions and
    /// assignment state first, since their own cleanup paths decide removal,
    /// then stopped work besides the idle restores of `executions`, then
    /// commands and leases, then the running agent of a coordinator that
    /// does its work.
    async fn recorded_cleanup_reason(
        &self,
        id: &SpaceId,
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
        // A saved chat in it comes back there, as a saved agent does.
        if self.chat_saved_in(id).await? {
            return Ok(Some(CleanupReason::Recovering));
        }
        if let Some(reason) = self.usage_reason(id, executions).await? {
            return Ok(Some(reason));
        }
        Ok(self
            .coordinator_runs(id)
            .await?
            .then_some(CleanupReason::ParentAgentRunning))
    }

    /// What keeps the workspace from every unattended removal, completion
    /// and PR cleanup included, read fresh on each call.
    pub(crate) async fn keep_reason(&self, id: &SpaceId) -> Result<Option<Retention>> {
        if self.has_holds(id).await? {
            return Ok(Some(Retention::Held));
        }
        if self.has_open_questions(id).await? {
            return Ok(Some(Retention::Question));
        }
        if self.has_pending_pull_proposal(id).await? {
            return Ok(Some(Retention::PrProposal));
        }
        if self.has_pending_push(id).await? {
            return Ok(Some(Retention::PushRequest));
        }
        if self.chat_runs_in(id).await? {
            return Ok(Some(Retention::ChatRunning));
        }
        if self.converting(id).await? {
            return Ok(Some(Retention::Converting));
        }
        Ok(self
            .has_queued_launch(id)
            .await?
            .then_some(Retention::QueuedAgent))
    }

    /// Assignment state whose own cleanup path decides removal.
    async fn assignment_reason(&self, id: &SpaceId) -> Result<Option<CleanupReason>> {
        let workspace = self.workspace_by_id(id).await?;
        if let Some(ancestor) = self.completed_ancestor(&workspace.id).await? {
            return Ok(Some(CleanupReason::AncestorCompleted {
                workspace: ancestor.name().clone(),
            }));
        }
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
        id: &SpaceId,
        executions: &CleanupExecutions,
    ) -> Result<Option<CleanupReason>> {
        if executions.running > 0 {
            return Ok(Some(CleanupReason::RunningCommands));
        }
        if !self
            .list_resources(Some(&SpaceSelector::id(id)))
            .await?
            .is_empty()
        {
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
}
