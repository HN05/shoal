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
mod claims;
mod costs;
mod ends;
mod listing;
mod overrun;
mod single_use;
mod taken;
mod waiting;

pub(crate) use shoal_runner::jobs::JobStop;
pub(crate) use shoal_runner::jobs::Registration;

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use futures_util::future::BoxFuture;
use rusqlite::{Connection, params};
use shoal_forge::{RepoRequests, account::Account};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_runner::{
    daemon::{Forges, Spaces, WaitingEntry},
    jobs::INTERVAL,
};
use tokio::time::Instant;

use crate::{
    daemon::{log, manager::Manager},
    forge::{ForgeRepo, items::reference::ItemRef},
    model::{QueuedJob, Repository, Space},
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
    manager.sample_job_usage().await?;
    manager.finish_ended_jobs().await?;
    manager.follow_taken_jobs().await?;
    manager.release_idle_runners().await?;
    claim_admitted_jobs(manager).await?;
    manager.poll_waiting_jobs().await
}

/// Claim each admitted job no claim runs for yet, each in a task of its own
/// that a daemon update waits for, and wake admission once it ends.
async fn claim_admitted_jobs(manager: &Arc<Manager>) -> anyhow::Result<()> {
    for entry in manager.admitted_jobs().await? {
        if !manager.runner.jobs().begin_claim(&entry.id) {
            continue;
        }
        let manager = manager.clone();
        let operation = manager.gates.owned_operation().await;
        tokio::spawn(async move {
            let _operation = operation;
            manager.claim_job(&entry).await;
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

    fn delete_waiting_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<bool> {
        super::agent_queue::actions::delete_waiting(db, id)
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

impl Spaces for Manager {
    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        Box::pin(Manager::present_spaces(self))
    }
}

#[cfg(test)]
mod tests;
