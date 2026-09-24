//! Explicit recovery: inspect first, mutate only requested and verified records.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};

use crate::{
    model::Workspace,
    process::{execution::Processes, identity as process},
    state::{ExecutionState, WorkspaceState, states},
    workspace::Manager,
};

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct ReconcileOptions {
    /// Apply safe state repairs instead of only reporting.
    pub repair: bool,
    /// Stop connected commands and identity-verified survivors first.
    pub stop: bool,
    /// The caller checked that untracked processes have stopped.
    pub acknowledge_stopped: bool,
}

states!(DirectoryState {
    Valid => "valid",
    Missing => "missing",
    Moved => "moved",
    Unverified => "unverified",
});

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

impl Report {
    fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            directory: DirectoryState::Unverified,
            moved_to: None,
            executions: vec![],
            changes: vec![],
            issues: vec![],
        }
    }

    /// A report for a workspace whose reconciliation itself failed.
    fn failed(workspace: Workspace, error: &anyhow::Error) -> Self {
        let mut report = Self::new(workspace);
        report.issues.push(format!("{error:#}"));
        report
    }
}

impl Manager {
    /// Startup audit never deletes work, releases leases, or assumes unknown
    /// executions are dead. Invalid ready workspaces are quarantined as failed.
    pub async fn audit_worktrees(&self) -> Result<()> {
        for workspace in self.list_workspaces().await? {
            if workspace.state != WorkspaceState::Ready {
                continue;
            }
            if let Err(error) = self.verify_worktree(&workspace).await {
                self.set_state(
                    &workspace.id,
                    WorkspaceState::Failed,
                    Some(format!(
                        "startup ownership check failed: {error:#}; run shoal doctor"
                    )),
                )
                .await?;
            } else if workspace.git_dir_id.is_none() {
                self.record_worktree_identity(&workspace).await?;
            }
        }
        Ok(())
    }

    /// Reconcile one workspace, or all of them. A failure for one workspace
    /// becomes an issue in its report rather than aborting the rest.
    pub async fn reconcile_workspaces(
        &self,
        selector: Option<&str>,
        options: ReconcileOptions,
    ) -> Result<Vec<Report>> {
        let workspaces = match selector {
            Some(selector) => vec![self.workspace(selector).await?],
            None => self.list_workspaces().await?,
        };
        let mut reports = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            reports.push(match self.reconcile(&workspace.id, options).await {
                Ok(report) => report,
                Err(error) => Report::failed(workspace, &error),
            });
        }
        Ok(reports)
    }

    pub async fn reconcile(&self, selector: &str, options: ReconcileOptions) -> Result<Report> {
        ensure!(
            options.repair || !(options.stop || options.acknowledge_stopped),
            "stop/acknowledge-stopped require --repair"
        );
        let workspace = self.workspace(selector).await?;
        // Serializes with branch creation and main updates; state reservation below
        // prevents new executions, removal, stop, and resource claims during repair.
        let gate = self.git_gate(&workspace.repository_id).await;
        let _guard = gate.lock().await;
        let mut report = Report::new(workspace.clone());
        if options.stop {
            // Connected wrappers handle their own foreground process groups.
            let running = self
                .inspect_workspace(&workspace.id)
                .await?
                .executions
                .iter()
                .any(|e| e.state == ExecutionState::Running);
            let quiescent = matches!(
                workspace.state,
                WorkspaceState::Ready | WorkspaceState::Failed
            );
            if running && quiescent {
                if let Err(error) = self.stop_workspace(&workspace.id).await {
                    report.issues.push(format!("connected stop: {error:#}"));
                }
            }
        }
        if options.repair {
            self.reserve_lifecycle(&workspace.id, WorkspaceState::Reconciling)
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
            report.workspace = self.workspace(&workspace.id).await?;
        }
        result?;
        Ok(report)
    }

    async fn reconcile_reserved(
        &self,
        workspace: &Workspace,
        options: ReconcileOptions,
        report: &mut Report,
    ) -> Result<()> {
        self.reconcile_directory(workspace, options, report).await?;
        let executions = self.inspect_workspace(&workspace.id).await?.executions;
        let ids: HashSet<_> = executions.iter().map(|e| e.id.clone()).collect();
        let mut scan = process::scan(ids.clone()).await?;
        for execution in executions {
            let entry = self
                .reconcile_execution(workspace, &execution, options, report, &mut scan, &ids)
                .await?;
            report.executions.push(entry);
        }
        if !options.repair && workspace.state != WorkspaceState::Ready && report.issues.is_empty() {
            report.issues.push(match &workspace.error {
                Some(error) => format!(
                    "{error}; alternatively, use --repair to restore verified state without retrying failed operations"
                ),
                None => "Workspace state requires repair; inspect this report then use --repair".into(),
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

    async fn reconcile_directory(
        &self,
        workspace: &Workspace,
        options: ReconcileOptions,
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
            return Ok(());
        }
        report.directory = DirectoryState::Missing;
        match self.missing_worktree(workspace).await {
            Ok(Some(path)) => {
                report.directory = DirectoryState::Moved;
                report.moved_to = Some(path.clone());
                report.issues.push(format!(
                    "Worktree moved to {}; restore it to the recorded path before using Shoal",
                    path.display()
                ));
            }
            Ok(None) => report.issues.push(
                "Workspace directory was deleted; the next cleanup sweep or shoal rm forgets the workspace and retains its branch".into(),
            ),
            Err(error) => report
                .issues
                .push(format!("Missing worktree cannot be verified: {error:#}")),
        }
        Ok(())
    }

    /// Inspect one recorded execution, optionally stopping and clearing it.
    async fn reconcile_execution(
        &self,
        workspace: &Workspace,
        execution: &crate::model::Execution,
        options: ReconcileOptions,
        report: &mut Report,
        scan: &mut process::Scan,
        ids: &HashSet<String>,
    ) -> Result<ExecutionReport> {
        let connected = self.execution_connected(&execution.id).await;
        let mut processes = Processes::inspect(execution, scan).await?;
        let mut notes = Vec::new();
        if options.stop && !connected {
            if processes.stop().await? {
                report.changes.push(format!(
                    "Stopped recorded survivors of execution {}",
                    execution.id
                ));
            }
            // A surviving process can fork while stopping. Rescan rather than
            // assuming that signaling the original list completed the job.
            *scan = process::scan(ids.clone()).await?;
            processes = Processes::inspect(execution, scan).await?;
        }
        let unverified = processes.unverified();
        if connected {
            notes.push("Execution is connected; not a stale record".into());
        }
        if !connected && processes.wrapper.is_some() {
            notes.push("Recorded execution wrapper is still alive".into());
        }
        if !processes.owned.is_empty() {
            notes.push(format!(
                "{} owned process(es) remain",
                processes.owned.len()
            ));
        }
        if !processes.launch_recorded {
            notes.push("Execution has incomplete launch identity; explicit --acknowledge-stopped is required after checking its processes".into());
        }
        if processes.unreadable > 0 {
            notes.push(format!(
                "{} same-user process environments could not be inspected",
                processes.unreadable
            ));
        }
        if !unverified.is_empty() {
            notes.push("Unverified process-group survivors require manual inspection; Shoal will not signal them".into());
        }
        let mut cleared = false;
        let safe = !connected
            && !processes.has_survivors()
            && (processes.visibility_complete() || options.acknowledge_stopped);
        if options.repair && safe {
            // Even an explicit acknowledgement cannot ignore visible cwd users.
            let cwd_users = if workspace.path.is_dir() {
                crate::process::in_directory(&workspace.path).await?
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
        Ok(ExecutionReport {
            id: execution.id.clone(),
            state: execution.state,
            connected,
            wrapper_alive: processes.wrapper.is_some(),
            processes: processes.owned,
            unverified_processes: unverified,
            cleared,
            notes,
        })
    }
}
