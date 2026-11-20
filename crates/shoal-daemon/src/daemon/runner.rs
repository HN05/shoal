//! Running repositories' CI jobs on this machine: the runner each
//! repository registers on its forge, the forge's waiting jobs as entries of
//! the agent queue, and the jobs the daemon claims, each in a workspace at its
//! commit that goes when the job ends.
//!
//! The runner's background work: each repository's runner follows its
//! `runner.enabled` setting, checked when the global config or a local
//! config changes and every [`INTERVAL`] for the repositories' own files; every
//! [`POLL_INTERVAL`], and whenever admission admits a job, the jobs that
//! ended give up their workspaces, admitted jobs are claimed, and the
//! forge's waiting jobs are read while one could start, and every
//! [`INTERVAL`] while none can.
mod ends;
mod job_workspaces;

pub(crate) use shoal_runner::jobs::JobStop;

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, params};
use shoal_forge::{RepoRequests, account::Account};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceName;
use shoal_runner::{
    daemon::{Forges, RecordedExecutions, Spaces, WaitingEntry},
    jobs::INTERVAL,
};
use tokio::time::Instant;

use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        events::StoppedProcess,
        executions::{Save, StopRequest},
        log,
        manager::Manager,
        space_processes::Selection,
    },
    forge::{ForgeRepo, items::reference::ItemRef},
    model::{QueuedJob, Repository, Space, Workspace, WorkspaceUsage},
    state::{QueueKind, QueueState},
};

/// How often the forge's waiting jobs are read while one could start.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

pub(super) async fn run(manager: Arc<Manager>) {
    let mut config = manager.config_state.watch();
    let mut reconciled: Option<Instant> = None;
    loop {
        {
            // A daemon update waits for the pass, as for any background
            // operation, and an exit that waits holds it back.
            let _operation = manager.gates.background_pass().await;
            let changed = manager.runner.jobs().take_changed();
            if changed {
                // A changed setting deserves a try at once.
                manager.runner.jobs().forget_failures();
            }
            if changed || reconciled.is_none_or(|at| at.elapsed() >= INTERVAL) {
                if let Err(error) = manager.runner.reconcile_runners(&*manager).await {
                    log!("runner: {error:#}");
                }
                reconciled = Some(Instant::now());
            }
            if let Err(error) = pass(&manager).await {
                log!("runner: {error:#}");
            }
        }
        tokio::select! {
            _ = config.changed() => {
                manager.runner.jobs().forget_failures();
                reconciled = None;
            }
            () = manager.runner.jobs().woken() => {}
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// Record the running jobs' usage, remove the workspaces of the jobs that
/// ended, move a job's workspace to the commit of the job its runner took
/// instead, release the runners the forge gave no job, claim the jobs
/// admission admitted, and read the forge's waiting jobs when due.
async fn pass(manager: &Arc<Manager>) -> anyhow::Result<()> {
    manager.runner.sample_job_usage(&**manager).await?;
    manager.runner.finish_ended_jobs(&**manager).await?;
    manager.runner.follow_taken_jobs(&**manager).await?;
    manager.runner.release_idle_runners(&**manager).await?;
    claim_admitted_jobs(manager).await?;
    manager.runner.poll_waiting_jobs(&**manager).await
}

/// Claim each admitted job no claim runs for yet, each in a task of its own
/// that a daemon update waits for, and wake admission once it ends.
async fn claim_admitted_jobs(manager: &Arc<Manager>) -> anyhow::Result<()> {
    for entry in manager.runner.admitted_jobs(&**manager).await? {
        if !manager.runner.jobs().begin_claim(&entry.id) {
            continue;
        }
        let manager = manager.clone();
        let operation = manager.gates.owned_operation().await;
        tokio::spawn(async move {
            let _operation = operation;
            manager.runner.claim_job(&*manager, &entry).await;
            manager.runner.jobs().end_claim(&entry.id);
            manager.agent_queue.wake();
        });
    }
    Ok(())
}

/// The agent queue's rows the runner changes in its own transactions.
pub(crate) struct RunnerRecords;

impl shoal_runner::daemon::Records for RunnerRecords {
    fn forget_job_entries(&self, db: &Connection, repository: &RepositoryId) -> Result<()> {
        db.execute(
            "DELETE FROM agent_queue WHERE repository_id=?1 AND kind=?2",
            params![repository, QueueKind::Job],
        )?;
        Ok(())
    }

    fn waiting_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        let rows = db
            .prepare(
                "SELECT id,repository_id,job FROM agent_queue
                 WHERE kind=?1 AND state=?2 AND job IS NOT NULL",
            )?
            .query_map(params![QueueKind::Job, QueueState::Queued], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(QueueEntryId, RepositoryId, String)>>>()?;
        let mut jobs = Vec::new();
        for (id, repository, job) in rows {
            jobs.push((id, repository, serde_json::from_str(&job)?));
        }
        Ok(jobs)
    }

    fn waiting_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
    ) -> Result<Vec<WaitingEntry>> {
        let entries = db
            .prepare(
                "SELECT id,name,job FROM agent_queue
                 WHERE repository_id=?1 AND kind=?2 AND state=?3",
            )?
            .query_map(
                params![repository, QueueKind::Job, QueueState::Queued],
                |row| {
                    let job = crate::daemon::store::json_column(row, "job")?;
                    Ok((row.get("id")?, row.get("name")?, job))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((entries.into_iter())
            .filter_map(|(id, name, job)| {
                Some(WaitingEntry {
                    id,
                    name,
                    job: job?,
                })
            })
            .collect())
    }

    fn admitted_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        let rows = db
            .prepare(
                "SELECT id,repository_id,job FROM agent_queue
                 WHERE kind=?1 AND state=?2 AND job IS NOT NULL",
            )?
            .query_map(params![QueueKind::Job, QueueState::Starting], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(QueueEntryId, RepositoryId, String)>>>()?;
        let mut jobs = Vec::new();
        for (id, repository, job) in rows {
            jobs.push((id, repository, serde_json::from_str(&job)?));
        }
        Ok(jobs)
    }

    fn recorded_executions(&self, db: &Connection) -> Result<RecordedExecutions> {
        let mut recorded = RecordedExecutions::default();
        let rows = db
            .prepare("SELECT id,space_id FROM executions")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(ExecutionId, SpaceId)>>>()?;
        for (id, space) in rows {
            recorded.ids.insert(id);
            recorded.spaces.insert(space);
        }
        Ok(recorded)
    }

    fn remove_queue_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()> {
        db.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
        Ok(())
    }

    fn return_job_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()> {
        db.execute(
            "UPDATE agent_queue SET state=?2 WHERE id=?1 AND state=?3",
            params![id, QueueState::Queued, QueueState::Starting],
        )?;
        Ok(())
    }

    fn delete_waiting_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<bool> {
        super::agent_queue::actions::delete_waiting(db, id)
    }

    fn forget_unlisted_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
        listed: &HashSet<String>,
    ) -> Result<HashSet<String>> {
        let entries = db
            .prepare(
                "SELECT id,json_extract(job,'$.job.handle'),state FROM agent_queue
                 WHERE repository_id=?1 AND kind=?2",
            )?
            .query_map(params![repository, QueueKind::Job], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(String, String, QueueState)>>>()?;
        let mut kept = HashSet::new();
        for (id, handle, state) in entries {
            if state == QueueState::Queued && !listed.contains(&handle) {
                db.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
            } else {
                kept.insert(handle);
            }
        }
        Ok(kept)
    }

    fn queue_job(&self, db: &Connection, repository: &RepositoryId, job: &QueuedJob) -> Result<()> {
        let name = SpaceName::derive(&format!("ci-{}-{}", job.job.name, job.job.id), "workspace");
        db.execute(
            "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job)
             VALUES (?1,?2,?3,?4,'',?5,?6,?7)",
            params![
                QueueEntryId::generate(),
                QueueKind::Job,
                repository,
                name,
                QueueState::Queued,
                crate::time::UnixTime::now(),
                serde_json::to_string(job)?,
            ],
        )?;
        Ok(())
    }
}

impl Forges for Manager {
    fn user_account(&self) -> Account {
        Manager::user_account(self)
    }

    fn checkout_forge<'a>(
        &'a self,
        repository: &'a Repository,
    ) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(Manager::checkout_forge(self, repository))
    }

    fn resolve_forge<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move { ForgeRepo::resolve(url, &self.forge_hosts()).await })
    }

    fn item_forge<'a>(&'a self, item: &'a ItemRef) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(Manager::item_forge(self, item))
    }
}

impl shoal_runner::daemon::Executions for Manager {
    fn stop_job_processes<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<StoppedProcess>>> {
        Box::pin(async move {
            let workspace = self.workspace_by_id(id).await?;
            (self.stop_workspace_untracked(&workspace, Selection::Processes, None)).await
        })
    }

    fn execution_recorded<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        let id = id.to_owned();
        Box::pin(self.store.run(move |db| {
            crate::daemon::store::exists(db, "SELECT 1 FROM executions WHERE id=?1", [id])
        }))
    }

    fn workspace_has_executions<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::workspace_has_executions(self, id))
    }

    fn job_execution_runs<'a>(
        &'a self,
        workspace: &'a SpaceId,
        id: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(Manager::job_execution_runs(self, workspace, id))
    }

    fn workspace_execution<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<ExecutionId>>> {
        Box::pin(async move {
            let executions = self.workspace_executions(id).await?;
            Ok(executions.into_iter().next().map(|execution| execution.id))
        })
    }

    fn launch_job_command<'a>(
        &'a self,
        workspace: &'a Workspace,
        log: std::path::PathBuf,
        command: shoal_forge::ci::runner::JobCommand,
    ) -> BoxFuture<'a, Result<ExecutionId>> {
        Box::pin(Manager::launch_job_command(self, workspace, log, command))
    }

    fn stop_job_runner<'a>(&'a self, id: &'a SpaceId, reason: String) -> BoxFuture<'a, Result<()>> {
        let request = StopRequest {
            reason,
            save: Save::Nothing,
        };
        Box::pin(async move { self.stop_space(&SpaceSelector::id(id), request).await })
    }
}

impl Spaces for Manager {
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        Box::pin(Manager::present_spaces(self))
    }

    fn workspace_usage(&self, id: &SpaceId) -> Option<WorkspaceUsage> {
        Manager::workspace_usage(self, id)
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(Manager::workspace_by_id(self, id))
    }

    fn select_space_id<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<SpaceId>> {
        Box::pin(async move { Ok(self.space(selector).await?.id) })
    }
}

#[cfg(test)]
mod tests;
