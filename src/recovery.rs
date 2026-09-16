//! Explicit recovery: inspect first, mutate only requested and verified records.
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};

use crate::{
    model::Workspace,
    process_identity as process,
    state::{ExecutionState, WorkspaceState},
    workspace::Manager,
    worktrunk,
};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct Options {
    pub repair: bool,
    pub stop: bool,
    pub acknowledge_stopped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryState {
    Valid,
    Missing,
    Moved,
    Unverified,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub id: String,
    pub state: ExecutionState,
    pub connected: bool,
    pub wrapper_alive: bool,
    pub processes: Vec<process::Identity>,
    pub unverified_processes: Vec<process::Identity>,
    pub cleared: bool,
    pub notes: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub workspace: Workspace,
    pub directory: DirectoryState,
    pub moved_to: Option<PathBuf>,
    pub executions: Vec<ExecutionReport>,
    pub changes: Vec<String>,
    pub issues: Vec<String>,
}

impl Manager {
    /// Startup audit never deletes work, releases leases, or assumes unknown
    /// executions are dead. Invalid ready workspaces are quarantined as failed.
    pub async fn audit_worktrees(&self) -> Result<()> {
        for workspace in self.list().await? {
            if workspace.state != WorkspaceState::Ready {
                continue;
            }
            if let Err(error) = self.verify_worktree(&workspace).await {
                self.set_state(
                    &workspace.id,
                    WorkspaceState::Failed,
                    Some(format!(
                        "startup ownership check failed: {error:#}; run shoal reconcile"
                    )),
                )
                .await?;
            } else if workspace.git_dir_id.is_none() {
                self.record_worktree_identity(&workspace).await?;
            }
        }
        Ok(())
    }

    pub async fn reconcile(&self, selector: String, options: Options) -> Result<Report> {
        ensure!(
            options.repair || !(options.stop || options.acknowledge_stopped),
            "stop/acknowledge-stopped require --repair"
        );
        let workspace = self.get(selector).await?;
        // Serializes with branch creation and main updates; state reservation below
        // prevents new executions, removal, stop, and resource claims during repair.
        let gate = self.git_gate(&workspace.repository_id).await;
        let _guard = gate.lock().await;
        let mut report = Report {
            workspace: workspace.clone(),
            directory: DirectoryState::Unverified,
            moved_to: None,
            executions: vec![],
            changes: vec![],
            issues: vec![],
        };
        if options.stop {
            // Connected wrappers handle their own foreground process groups.
            if self
                .inspect(workspace.id.clone())
                .await?
                .executions
                .iter()
                .any(|e| e.state == ExecutionState::Running)
                && matches!(
                    workspace.state,
                    WorkspaceState::Ready | WorkspaceState::Failed
                )
            {
                if let Err(error) = self.stop(workspace.id.clone()).await {
                    report.issues.push(format!("connected stop: {error:#}"));
                }
            }
        }
        if options.repair {
            let id = workspace.id.clone();
            self.store
                .run(move |db| {
                    ensure!(
                        db.execute(
                            "UPDATE workspaces SET state=?2 WHERE id=?1 AND state IN (?3,?4)",
                            params![
                                id,
                                WorkspaceState::Reconciling,
                                WorkspaceState::Ready,
                                WorkspaceState::Failed
                            ]
                        )? == 1,
                        "workspace has another lifecycle operation in progress"
                    );
                    Ok(())
                })
                .await?;
        }
        let result = self
            .reconcile_reserved(&workspace, options, &mut report)
            .await;
        if options.repair {
            let state = if result.is_err() {
                workspace.state
            } else if matches!(report.directory, DirectoryState::Valid) && report.issues.is_empty()
            {
                WorkspaceState::Ready
            } else {
                WorkspaceState::Failed
            };
            let error = result
                .as_ref()
                .err()
                .map(|e| format!("reconciliation failed: {e:#}"))
                .or_else(|| (!report.issues.is_empty()).then(|| report.issues.join("; ")));
            self.set_state(&workspace.id, state, error).await?;
            self.touch(&workspace.id).await;
            report.workspace = self.get(workspace.id).await?;
        }
        result?;
        Ok(report)
    }

    async fn reconcile_reserved(
        &self,
        workspace: &Workspace,
        options: Options,
        report: &mut Report,
    ) -> Result<()> {
        if workspace.path.try_exists()? {
            match self.verify_worktree(workspace).await {
                Ok(()) => {
                    report.directory = DirectoryState::Valid;
                    if options.repair && workspace.git_dir_id.is_none() {
                        self.record_worktree_identity(workspace).await?;
                        report.changes.push("Recorded Git worktree identity".into());
                    }
                }
                Err(error) => report
                    .issues
                    .push(format!("Worktree ownership cannot be verified: {error:#}")),
            }
        } else {
            report.directory = DirectoryState::Missing;
            match self.missing_worktree(workspace).await {
                Ok(Some(path)) => {
                    report.directory = DirectoryState::Moved;
                    report.moved_to = Some(path.clone());
                    report.issues.push(format!("Worktree moved to {}; restore it to the recorded path before using Shoal", path.display()));
                }
                Ok(None) => report.issues.push("Workspace directory is missing; after repair, use shoal rm to finish owned-resource cleanup (branch retained)".into()),
                Err(error) => report.issues.push(format!("Missing worktree cannot be verified: {error:#}")),
            }
        }
        if !options.repair && workspace.state != WorkspaceState::Ready {
            report.issues.push(
                "Workspace state requires repair; inspect this report then use --repair".into(),
            );
        }
        let executions = self.inspect(workspace.id.clone()).await?.executions;
        let ids: HashSet<_> = executions.iter().map(|e| e.id.clone()).collect();
        let mut scan = process::scan(ids.clone()).await?;
        for execution in executions {
            let connected = self
                .active
                .lock()
                .await
                .get(&execution.id)
                .is_some_and(|s| !s.is_closed());
            let mut wrapper_alive = execution
                .wrapper
                .as_ref()
                .map(process::alive)
                .transpose()?
                .unwrap_or(false);
            let mut owned: Vec<_> = scan
                .processes
                .iter()
                .filter(|p| p.execution_id == execution.id)
                .map(|p| p.identity.clone())
                .collect();
            let (related, mut unverified) =
                process::related(execution.child.as_ref(), execution.group_id).await?;
            owned.extend(related);
            if let Some(child) = &execution.child {
                if process::alive(child)? {
                    owned.push(child.clone());
                }
            }
            owned.sort_by_key(|p| p.pid);
            owned.dedup_by_key(|p| p.pid);
            unverified.retain(|p| !owned.contains(p));
            let mut notes = Vec::new();
            if options.stop && !connected {
                let mut targets = owned.clone();
                if wrapper_alive {
                    targets.push(execution.wrapper.clone().unwrap());
                }
                process::stop_verified(&targets).await?;
                if !targets.is_empty() {
                    report.changes.push(format!(
                        "Stopped recorded survivors of execution {}",
                        execution.id
                    ));
                }
                // A surviving process can fork while stopping. Rescan rather than
                // assuming that signaling the original list completed the job.
                scan = process::scan(ids.clone()).await?;
                owned = scan
                    .processes
                    .iter()
                    .filter(|p| p.execution_id == execution.id)
                    .map(|p| p.identity.clone())
                    .collect();
                let (related, candidates) =
                    process::related(execution.child.as_ref(), execution.group_id).await?;
                owned.extend(related);
                if let Some(child) = &execution.child {
                    if process::alive(child)? {
                        owned.push(child.clone());
                    }
                }
                unverified = candidates
                    .into_iter()
                    .filter(|p| !owned.contains(p))
                    .collect();
                wrapper_alive = execution
                    .wrapper
                    .as_ref()
                    .map(process::alive)
                    .transpose()?
                    .unwrap_or(false);
            }
            let tracked = execution.wrapper.is_some() && execution.group_id.is_some();
            let unreadable = scan
                .unreadable
                .iter()
                .filter(|p| {
                    execution
                        .wrapper
                        .as_ref()
                        .is_none_or(|w| p.not_older_than(w))
                })
                .count();
            if connected {
                notes.push("Execution is connected; not a stale record".into());
            }
            if !connected && wrapper_alive {
                notes.push("Recorded execution wrapper is still alive".into());
            }
            if !owned.is_empty() {
                notes.push(format!("{} owned process(es) remain", owned.len()));
            }
            if !tracked {
                notes.push("Execution has incomplete launch identity; explicit --acknowledge-stopped is required after checking its processes".into());
            }
            if unreadable > 0 {
                notes.push(format!(
                    "{} same-user process environments could not be inspected",
                    unreadable
                ));
            }
            if !unverified.is_empty() {
                notes.push("Unverified process-group survivors require manual inspection; Shoal will not signal them".into());
            }
            let mut cleared = false;
            let safe = !connected
                && !wrapper_alive
                && owned.is_empty()
                && unverified.is_empty()
                && ((tracked && unreadable == 0) || options.acknowledge_stopped);
            if options.repair && safe {
                // Even an explicit acknowledgement cannot ignore visible cwd users.
                let cwd_users = if workspace.path.is_dir() {
                    crate::processes::in_directory(&workspace.path).await?
                } else {
                    vec![]
                };
                if cwd_users.is_empty() {
                    let id = execution.id.clone();
                    self.store
                        .run(move |db| {
                            db.execute("DELETE FROM executions WHERE id=?1", [&id])?;
                            Ok(())
                        })
                        .await?;
                    cleared = true;
                    report
                        .changes
                        .push(format!("Cleared stopped execution {}", execution.id));
                } else {
                    notes.push(format!(
                        "Processes still use the directory: {}",
                        cwd_users.join(", ")
                    ));
                }
            }
            if !connected && !cleared {
                report
                    .issues
                    .push(format!("Execution {} remains unresolved", execution.id));
            }
            report.executions.push(ExecutionReport {
                id: execution.id,
                state: execution.state,
                connected,
                wrapper_alive,
                processes: owned,
                unverified_processes: unverified,
                cleared,
                notes,
            });
        }
        if options.repair
            && matches!(report.directory, DirectoryState::Valid)
            && report.issues.is_empty()
            && workspace.state == WorkspaceState::Failed
        {
            report.changes.push("Restored verified worktree to ready; files, branches and resource leases preserved".into());
        }
        Ok(())
    }

    pub(crate) async fn missing_registration(&self, workspace: &Workspace) -> Result<bool> {
        let repo = self.repository(&workspace.repository_id).await?;
        let records =
            worktrunk::git(&repo.path, &["worktree", "list", "--porcelain", "-z"]).await?;
        let absolute = std::fs::canonicalize(
            workspace
                .path
                .parent()
                .context("missing workspace parent")?,
        )?
        .join(
            workspace
                .path
                .file_name()
                .context("missing workspace name")?,
        );
        Ok(records
            .split(' ')
            .filter_map(|f| f.strip_prefix("worktree "))
            .any(|p| {
                std::path::Path::new(p) == absolute || std::path::Path::new(p) == workspace.path
            }))
    }

    /// Missing worktrees may have been moved outside Shoal, not deleted. Consult
    /// their recorded admin directory before allowing ownership cleanup.
    pub(crate) async fn missing_worktree(&self, workspace: &Workspace) -> Result<Option<PathBuf>> {
        let repo = self.repository(&workspace.repository_id).await?;
        let records =
            worktrunk::git(&repo.path, &["worktree", "list", "--porcelain", "-z"]).await?;
        if let Some(directory) = &workspace.git_dir {
            if directory.try_exists()? {
                if let Some(identity) = &workspace.git_dir_id {
                    ensure!(
                        directory_identity(directory)? == *identity,
                        "Git worktree metadata was replaced; ownership cannot be verified"
                    );
                }
            }
            let link = directory.join("gitdir");
            match std::fs::read_to_string(&link) {
                Ok(destination) => {
                    let path = PathBuf::from(destination.trim_end_matches('\n'));
                    let path = path.parent().context("invalid Git worktree link")?;
                    if path.try_exists()? && std::fs::canonicalize(path)? != workspace.path {
                        return Ok(Some(path.to_owned()));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("read worktree ownership link"),
            }
        } else {
            // Older records cannot prove identity after a move; a branch match at
            // another existing path is enough to block forgetting the workspace.
            for record in records.split("\0\0") {
                if record
                    .split('\0')
                    .any(|f| f == format!("branch refs/heads/{}", workspace.branch))
                {
                    if let Some(path) = record.split('\0').find_map(|f| f.strip_prefix("worktree "))
                    {
                        if std::path::Path::new(path).is_dir() {
                            return Ok(Some(path.into()));
                        }
                    }
                }
            }
        }
        Ok(None)
    }
}

pub(crate) fn directory_identity(path: &std::path::Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    ensure!(metadata.is_dir(), "Git metadata is not a directory");
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}
