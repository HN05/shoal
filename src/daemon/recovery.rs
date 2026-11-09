//! Explicit recovery: inspect first, mutate only requested and verified records.
use crate::client::execution_ids::ExecutionId;
use anyhow::{Result, ensure};
use std::collections::HashSet;

use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::workspace::{Manager, StopRequest},
    fsutil::off_runtime,
    model::Workspace,
    process::{execution::Processes, identity as process},
    state::{ExecutionState, WorkspaceState},
};

pub(crate) use crate::client::recovery::{
    DirectoryState, ExecutionReport, ReconcileOptions, Report,
};
#[cfg(test)]
mod tests;

/// Operations on the client type [`Report`] that only the `shoal` command uses.
trait ReportExt {
    fn new(workspace: Workspace) -> Self;

    /// A report for a workspace whose reconciliation itself failed.
    fn failed(workspace: Workspace, error: &anyhow::Error) -> Self;
}

impl ReportExt for Report {
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

    fn failed(workspace: Workspace, error: &anyhow::Error) -> Self {
        let mut report = Self::new(workspace);
        report.issues.push(format!("{error:#}"));
        report
    }
}

impl Manager {
    /// Reconcile one workspace, or all of them. A failure for one workspace
    /// becomes an issue in its report rather than aborting the rest.
    pub async fn reconcile_workspaces(
        &self,
        selector: Option<&SpaceSelector>,
        options: ReconcileOptions,
    ) -> Result<Vec<Report>> {
        let workspaces = match selector {
            Some(selector) => vec![self.workspace(selector).await?],
            None => self.list_workspaces().await?,
        };
        let mut reports = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            reports.push(
                match self
                    .reconcile(&SpaceSelector::id(&workspace.id), options)
                    .await
                {
                    Ok(report) => report,
                    Err(error) => Report::failed(workspace, &error),
                },
            );
        }
        Ok(reports)
    }

    pub async fn reconcile(
        &self,
        selector: &SpaceSelector,
        options: ReconcileOptions,
    ) -> Result<Report> {
        Self::validate_reconcile_options(options)?;
        let queued = self.workspace(selector).await?;
        // Serializes with branch creation and main updates; state reservation below
        // prevents new executions, removal, stop, and resource claims during repair.
        let _guard = self.gates.lock_git(&queued.repository_id).await;
        // Read again: a repair that held the gate meanwhile may have changed it.
        let workspace = self.workspace_by_id(&queued.id).await?;
        let mut report = Report::new(workspace.clone());
        let reserved_from = self
            .prepare_reconcile(&workspace, options, &mut report)
            .await?;
        let result = self
            .reconcile_reserved(&workspace, options, &mut report)
            .await;
        if let Some(previous) = reserved_from {
            self.finalize_repair(&workspace, previous, &result, &mut report)
                .await?;
        }
        result?;
        Ok(report)
    }

    fn validate_reconcile_options(options: ReconcileOptions) -> Result<()> {
        ensure!(
            options.repair || !(options.stop || options.acknowledge_stopped || options.reclaim),
            "stopping, acknowledging stopped processes and reclaiming need repair"
        );
        Ok(())
    }

    /// Returns the state a repair reserved the lifecycle from.
    async fn prepare_reconcile(
        &self,
        workspace: &Workspace,
        options: ReconcileOptions,
        report: &mut Report,
    ) -> Result<Option<WorkspaceState>> {
        self.stop_for_reconcile(workspace, options, report).await?;
        if !options.repair {
            return Ok(None);
        }
        self.reserve_lifecycle(&workspace.id, WorkspaceState::Reconciling)
            .await
            .map(Some)
    }

    async fn stop_for_reconcile(
        &self,
        workspace: &Workspace,
        options: ReconcileOptions,
        report: &mut Report,
    ) -> Result<()> {
        if !options.stop {
            return Ok(());
        }
        // Connected wrappers handle their own foreground process groups.
        let running = self
            .workspace_executions(&workspace.id)
            .await?
            .iter()
            .any(|e| e.state == ExecutionState::Running);
        let quiescent = matches!(
            workspace.state,
            WorkspaceState::Ready | WorkspaceState::Failed
        );
        if running
            && quiescent
            && let Err(error) = self
                .stop_workspace(&SpaceSelector::id(&workspace.id), StopRequest::repair())
                .await
        {
            report.issues.push(format!("connected stop: {error:#}"));
        }
        Ok(())
    }

    async fn finalize_repair(
        &self,
        workspace: &Workspace,
        previous: WorkspaceState,
        result: &Result<()>,
        report: &mut Report,
    ) -> Result<()> {
        let state = if result.is_err() {
            previous
        } else if matches!(report.directory, DirectoryState::Valid) && report.issues.is_empty() {
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
        report.workspace = self.workspace_by_id(&workspace.id).await?;
        Ok(())
    }

    async fn reconcile_reserved(
        &self,
        workspace: &Workspace,
        options: ReconcileOptions,
        report: &mut Report,
    ) -> Result<()> {
        let workspace = if !off_runtime::try_exists(&workspace.path).await? {
            // No HEAD remains to reconcile. Shared removal verifies deletion,
            // releases leases and cascades the reserved rename name.
            workspace.clone()
        } else if options.repair {
            if self
                .finish_workspace_rename(workspace, options.reclaim)
                .await?
            {
                report
                    .changes
                    .push("Reconciled interrupted branch rename".into());
            }
            self.workspace_by_id(&workspace.id).await?
        } else {
            self.ensure_no_pending_rename(&workspace.id).await?;
            workspace.clone()
        };
        let workspace = &workspace;
        self.reconcile_directory(workspace, options, report).await?;
        let executions = self.workspace_executions(&workspace.id).await?;
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
                    "{error}; to keep the workspace as it is without retrying, accept a failed setup with shoal work setup --skip"
                ),
                None => format!(
                    "Workspace is {}; shoal work status shows why",
                    workspace.state
                ),
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
        if off_runtime::try_exists(&workspace.path).await? {
            match self
                .verify_and_record_worktree(workspace, options.repair)
                .await
            {
                Ok(verified) => {
                    report.directory = DirectoryState::Valid;
                    if verified.identity? {
                        report.changes.push("Recorded Git worktree identity".into());
                    }
                }
                Err(error) if options.reclaim => match self.reclaim_worktree(workspace).await {
                    Ok(()) => {
                        report.directory = DirectoryState::Valid;
                        report.changes.push(format!(
                            "Reclaimed the worktree at its recorded path after: {error:#}"
                        ));
                    }
                    Err(refused) => report.issues.push(format!(
                        "Worktree ownership cannot be verified: {error:#}; reclaim refused: {refused:#}"
                    )),
                },
                Err(error) => report.issues.push(format!(
                    "Worktree ownership cannot be verified: {error:#}; if {path} is still this workspace's worktree, reclaim it with shoal work adopt {path}",
                    path = workspace.path.display()
                )),
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
        ids: &HashSet<ExecutionId>,
    ) -> Result<ExecutionReport> {
        let connected = self.executions.is_connected(&execution.id).await;
        let mut processes = Processes::inspect(execution, &workspace.path, scan).await?;
        if options.stop && !connected {
            processes =
                stop_survivors(execution, &workspace.path, processes, report, scan, ids).await?;
        }
        let mut notes = execution_notes(connected, &processes);
        let mut cleared = false;
        if options.repair && clearable(connected, &processes, options.acknowledge_stopped) {
            match self
                .clear_stopped_execution(workspace, &execution.id)
                .await?
            {
                Clearance::Cleared => {
                    cleared = true;
                    report
                        .changes
                        .push(format!("Cleared stopped execution {}", execution.id));
                }
                Clearance::DirectoryInUse(users) => notes.push(format!(
                    "Processes still use the directory: {}",
                    users.join(", ")
                )),
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
            unverified_processes: processes.unverified(),
            processes: processes.owned,
            unreadable_processes: processes.unreadable,
            cleared,
            notes,
        })
    }

    /// Delete an execution record judged clearable, unless processes still use
    /// the workspace directory: even an explicit acknowledgement cannot ignore
    /// visible cwd users.
    async fn clear_stopped_execution(
        &self,
        workspace: &Workspace,
        id: &ExecutionId,
    ) -> Result<Clearance> {
        let users = if off_runtime::is_dir(&workspace.path).await? {
            crate::process::in_directory(&workspace.path).await?
        } else {
            vec![]
        };
        if !users.is_empty() {
            return Ok(Clearance::DirectoryInUse(users));
        }
        self.forget_execution(id).await?;
        Ok(Clearance::Cleared)
    }
}

enum Clearance {
    Cleared,
    DirectoryInUse(Vec<String>),
}

/// Stop the recorded survivors of a disconnected execution and inspect again.
async fn stop_survivors(
    execution: &crate::model::Execution,
    workspace: &std::path::Path,
    processes: Processes,
    report: &mut Report,
    scan: &mut process::Scan,
    ids: &HashSet<ExecutionId>,
) -> Result<Processes> {
    if processes.stop().await? {
        report.changes.push(format!(
            "Stopped recorded survivors of execution {}",
            execution.id
        ));
    }
    // A surviving process can fork while stopping. Rescan rather than
    // assuming that signaling the original list completed the job.
    *scan = process::scan(ids.clone()).await?;
    Processes::inspect(execution, workspace, scan).await
}

/// Whether the evidence proves the execution ended, so repair may clear it.
fn clearable(connected: bool, processes: &Processes, acknowledge_stopped: bool) -> bool {
    !connected
        && !processes.has_survivors()
        // An acknowledgement can cover a missing launch identity, but
        // never an environment that could not be inspected.
        && processes.unreadable.is_empty()
        && (processes.visibility_complete() || acknowledge_stopped)
}

/// Explain what keeps an execution from being a provably stale record.
fn execution_notes(connected: bool, processes: &Processes) -> Vec<String> {
    let mut notes = Vec::new();
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
        notes.push("Execution has incomplete launch identity; shoal stop asks you to confirm it ended after you check its processes".into());
    }
    if !processes.unreadable.is_empty() {
        notes.push(format!(
            "{} same-user process environments could not be inspected",
            processes.unreadable.len()
        ));
    }
    if !processes.unverified().is_empty() {
        notes.push("Unverified process-group survivors require manual inspection; Shoal will not signal them".into());
    }
    notes
}
