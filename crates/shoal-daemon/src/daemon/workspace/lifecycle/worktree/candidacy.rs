//! The worktree's part of cleanup candidacy: its commands and leases, its
//! Git state and files, and the processes working in it.
use super::super::super::Manager;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::cleanup::{Rule, Tree},
    fsutil::off_runtime,
    git::{self, default_branch::DefaultBranchLookup},
    model::{CleanupReason, Workspace},
    process,
    removal::{self, IdleRestore, Inspection, InspectionPolicy, RemovalCheck},
    state::WorkspaceState,
};
use anyhow::Result;
use shoal_refs::BranchName;
use std::{collections::HashMap, path::Path};

/// Why cleanup leaves `workspace` alone whatever else it finds: its worktree
/// is not ready, as while it is set up or removed.
pub(crate) fn not_ready(workspace: &Workspace) -> Option<CleanupReason> {
    (workspace.state != WorkspaceState::Ready).then_some(CleanupReason::NotReady {
        state: workspace.state,
    })
}

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

/// Whether a process `users` finds is active in the directory, besides the
/// wrappers waiting to restore the idle agents of `executions`.
async fn directory_in_use(
    path: &Path,
    executions: &CleanupExecutions,
    users: &process::DirectoryUsers,
) -> Result<bool> {
    let wrappers = removal::wrappers(&executions.idle);
    Ok(!users.activity_except(path, &wrappers).await?.is_empty())
}

/// The workspace's executions as cleanup sees them.
pub(in crate::daemon::workspace) struct CleanupExecutions {
    /// Those other than the idle restores, which keep the workspace.
    running: usize,
    /// The restores of idle agents, with those a removal stopped.
    idle: Vec<IdleRestore>,
}

/// HEAD of the worktree at `path`.
async fn head(path: &Path) -> Result<String> {
    git::revision(path, "HEAD", git::run).await
}

impl Manager {
    pub(in crate::daemon::workspace) async fn inspect_removal(
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
            default_branch.as_ref().map(BranchName::as_str),
        )
        .await
    }

    /// The workspace's executions, telling the restores of idle agents, which
    /// keep it from no cleanup, from the others. `cancelled` are the idle
    /// restores a removal stopped, whose records and exiting wrappers it
    /// still disregards.
    pub(in crate::daemon::workspace) async fn cleanup_executions(
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

    /// HEAD of the worktree of `workspace` as an idle cleanup candidate, or
    /// the first reason it keeps it, inspected under
    /// [`Manager::check_access`]: commands and leases, its Git state, then
    /// the processes in it, read from `users`, since Git state rules out
    /// most worktrees. `cancelled` are the restores a removal stopped.
    pub(in crate::daemon::workspace) async fn idle_worktree(
        &self,
        workspace: &Workspace,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Result<String, CleanupReason>> {
        let inspected = async {
            let executions = self.cleanup_executions(&workspace.id, cancelled).await?;
            if let Some(reason) = self.usage_reason(&workspace.id, &executions).await? {
                return Ok(Err(reason));
            }
            let mut check =
                (self.inspect_removal(workspace.clone(), InspectionPolicy::GitOnly, executions))
                    .await?;
            if let Some(reason) = worktree_cleanup_reason(&check).await? {
                return Ok(Err(reason));
            }
            check.load_directory_processes(users).await?;
            if let Some(reason) = worktree_cleanup_reason(&check).await? {
                return Ok(Err(reason));
            }
            Ok(Ok(head(&workspace.path).await?))
        };
        self.check_access(&workspace.path, inspected).await
    }

    /// HEAD of the worktree of `workspace` as a merged cleanup candidate,
    /// inspected under [`Manager::check_access`], or `None` when it is not
    /// one: unused, merged into the default branch, with no Git operation
    /// under way, no workspace stacked on it and any unsaved files left over
    /// from merged work. `cancelled` are the restores a removal stopped, and
    /// the processes in the worktree are read from `users`.
    pub(in crate::daemon::workspace) async fn merged_worktree(
        &self,
        workspace: &Workspace,
        cancelled: &[IdleRestore],
        users: &process::DirectoryUsers,
    ) -> Result<Option<String>> {
        let inspected = async {
            let executions = self.cleanup_executions(&workspace.id, cancelled).await?;
            if self
                .usage_reason(&workspace.id, &executions)
                .await?
                .is_some()
            {
                return Ok(None);
            }
            let mut check =
                (self.inspect_removal(workspace.clone(), InspectionPolicy::GitOnly, executions))
                    .await?;
            if !off_runtime::is_dir(&workspace.path).await?
                || !Rule::Merged.allows(&check)
                || git::status::operation_in_progress(&workspace.path).await?
                || (check.dirty && !self.unsaved_files_are_leftovers(&check.workspace).await?)
            {
                return Ok(None);
            }
            check.load_directory_processes(users).await?;
            if !Rule::Merged.allows(&check) {
                return Ok(None);
            }
            Ok(Some(head(&workspace.path).await?))
        };
        self.check_access(&workspace.path, inspected).await
    }

    /// HEAD of the worktree of `workspace` whose idle timer runs, inspected
    /// under [`Manager::check_access`], or `None` when its commands, leases
    /// or a process `users` finds in it keep it.
    pub(in crate::daemon::workspace) async fn idle_timer_worktree(
        &self,
        workspace: &Workspace,
        users: &process::DirectoryUsers,
    ) -> Result<Option<String>> {
        let inspected = async {
            let executions = self.cleanup_executions(&workspace.id, &[]).await?;
            if self
                .usage_reason(&workspace.id, &executions)
                .await?
                .is_some()
                || !off_runtime::is_dir(&workspace.path).await?
                || directory_in_use(&workspace.path, &executions, users).await?
            {
                return Ok(None);
            }
            Ok(Some(head(&workspace.path).await?))
        };
        self.check_access(&workspace.path, inspected).await
    }

    /// One walk of the files in the worktree of `workspace`.
    pub(in crate::daemon::workspace) async fn walk_worktree(
        &self,
        workspace: &Workspace,
    ) -> Result<Tree> {
        let path = workspace.path.clone();
        tokio::task::spawn_blocking(move || crate::daemon::cleanup::walk(&path)).await?
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
