//! Dispatch: each pass ends abandoned starts and restores, queues the
//! restores stopped agents request, then admits the waiting entries that fit
//! and starts what the daemon starts itself.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_names::SpaceName;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::params;

use super::{
    dependencies, expiry,
    limits::{Waiting, Wake, Work, admit},
    store::{QueuedRow, queued_rows},
};
use crate::client::time::UnixTime;
use crate::{
    daemon::{notifications::NotificationKind, workspace::Manager},
    state::{QueueKind, QueueState},
};

/// How long a starting entry keeps its slot without a holder: long enough for
/// a launch to hand over to a Herdr tab or reconnect after a daemon restart.
pub(super) const START_GRACE: Duration = Duration::from_secs(120);

/// An admitted entry the daemon starts itself.
pub(crate) struct Start {
    pub id: String,
    pub name: SpaceName,
    pub env: BTreeMap<String, String>,
}

/// The waiting entries as admission sees them.
struct WaitingWork {
    entries: Vec<Waiting>,
    /// Each restore a waiting wrapper carries out, by entry ID, with its
    /// execution and wake.
    restores: HashMap<String, (ExecutionId, Wake)>,
    /// The executions of the stopped agents whose restore the daemon starts,
    /// which hold no slot until it is admitted.
    stopped: HashSet<ExecutionId>,
}

impl WaitingWork {
    /// Whether `admitted` holds an entry whose work is of `kind`.
    fn admits(&self, admitted: &[String], kind: fn(&Work) -> bool) -> bool {
        self.entries
            .iter()
            .any(|entry| kind(&entry.work) && admitted.contains(&entry.id))
    }

    /// The execution and wake of each restore in `admitted` that a waiting
    /// wrapper carries out.
    fn admitted_restores<'a>(
        &'a self,
        admitted: &'a [String],
    ) -> impl Iterator<Item = &'a (ExecutionId, Wake)> {
        admitted.iter().filter_map(|id| self.restores.get(id))
    }

    /// Whether `admitted` holds an entry that waited for healthy readings: a
    /// restore of an agent stopped mid-turn or a one-shot run protection
    /// stopped.
    fn needs_recovery(&self, admitted: &[String]) -> bool {
        self.admitted_restores(admitted)
            .any(|(_, wake)| *wake == Wake::Recovery)
            || self.admits(admitted, |work| *work == Work::OneShot { after_stop: true })
    }
}

/// The executions of the `waiting` restores that wait for an update or only
/// a slot with every process of their agent proven stopped, which hold no
/// slot. One whose processes may still run keeps its slot until they stop.
fn dormant_restores(
    waiting: &[Waiting],
    restores: &HashMap<String, (ExecutionId, Wake)>,
) -> HashSet<ExecutionId> {
    waiting
        .iter()
        .filter(|entry| {
            matches!(
                entry.work,
                Work::Restore {
                    waiting: true,
                    wake: Wake::Update { .. } | Wake::Slot,
                }
            )
        })
        .filter_map(|entry| restores.get(&entry.id))
        .map(|(execution, _)| execution.clone())
        .collect()
}

impl Manager {
    /// End abandoned starts and restores and those resolved another way,
    /// with the restarts they started, queue the restores stopped agents
    /// request, then start the waiting entries that fit.
    pub(crate) async fn dispatch_queue(&self) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        self.expire_starting_entries().await?;
        self.expire_orphaned_restores().await?;
        self.forget_resolved_restores().await?;
        self.fail_lost_renewals().await?;
        self.queue_requested_restores().await?;
        self.admit_waiting(None).await?;
        Ok(())
    }

    /// Admit waiting entries that fit, start launches other than `caller`'s,
    /// and return whether `caller`'s entry was admitted. An admitted restore
    /// is left to its waiting agent; nothing is admitted while the system
    /// shuts down. Requires the queue gate.
    pub(super) async fn admit_waiting(&self, caller: Option<&str>) -> Result<bool> {
        if self.agent_queue.suspended() {
            return Ok(false);
        }
        let mut work = self.waiting_work().await?;
        let bases = self.hold_back_dependents(&mut work.entries).await?;
        if work.entries.is_empty() {
            return Ok(false);
        }
        let published = self.overload_recovery.published();
        let admitted = self.fitting_entries(&work, published.is_some()).await?;
        let Some(admitted) = self.grant_admission(&work, admitted, published) else {
            return Ok(false);
        };
        let starts = self.mark_starting(admitted, bases).await?;
        self.start_launches(starts, caller).await
    }

    /// The entries of `work` that fit beside the slots in use, where
    /// `recovered` says whether healthy readings are published.
    async fn fitting_entries(&self, work: &WaitingWork, recovered: bool) -> Result<Vec<String>> {
        let repositories = work
            .entries
            .iter()
            .map(|entry| entry.repository.clone())
            .collect();
        let mut dormant = dormant_restores(&work.entries, &work.restores);
        dormant.extend(work.stopped.iter().cloned());
        let slots = self.occupied_slots(&dormant).await?;
        let jobs = self.job_activities().await?;
        let limits = self.agent_limits(repositories, &slots, &jobs).await?;
        let occupied: Vec<RepositoryId> = slots.into_iter().map(|slot| slot.repository).collect();
        Ok(admit(&occupied, &work.entries, &limits, recovered))
    }

    /// Grant the `admitted` entries of `work` their start: wake the runners
    /// for a CI job, let each restore's waiting agent go and give one-shot
    /// runs their slots, using up the recovery epoch `published` for those
    /// that waited for healthy readings. Return the entries left to mark
    /// starting, or none when a stop, reload or unsafe reading withdrew the
    /// readings since; the pass it wakes decides again.
    fn grant_admission(
        &self,
        work: &WaitingWork,
        admitted: Vec<String>,
        published: Option<u64>,
    ) -> Option<Vec<String>> {
        if work.admits(&admitted, |work| matches!(work, Work::Job { .. })) {
            self.runners.wake();
        }
        let epoch = match work.needs_recovery(&admitted) {
            true => Some(published.and_then(|epoch| self.overload_recovery.consume(epoch))?),
            false => None,
        };
        for (execution, wake) in work.admitted_restores(&admitted) {
            if let Some(grant) = wake.grant(epoch) {
                self.agent_queue.grant_restore(execution, grant);
            }
        }
        Some(
            admitted
                .into_iter()
                .filter(|id| !self.agent_queue.admit_oneshot(id))
                .collect(),
        )
    }

    /// The queued entries in order, each restore with whether its agent
    /// waits and what for. A restore the daemon starts is admitted like a
    /// launch once it waits for nothing. One-shot runs, such as reviews of
    /// work already done, go ahead of every launch.
    async fn waiting_work(&self) -> Result<WaitingWork> {
        let queued = self.store.run(|db| queued_rows(db)).await?;
        let waiters = self.agent_queue.restore_waiters();
        let mut restores = HashMap::new();
        let mut stopped = HashSet::new();
        let mut waiting = Vec::new();
        for QueuedRow {
            id,
            repository,
            kind,
            execution,
            workspace,
            started_here,
            automatic,
            job,
        } in queued
        {
            let work = match (kind, execution, workspace) {
                // A job waits while its repository's permits are all held.
                (QueueKind::Job, ..) if self.runners.permits_busy(&repository) => continue,
                (QueueKind::Job, ..) => {
                    let job = serde_json::from_str(&job.context("a job's entry names no job")?)?;
                    Work::Job {
                        cost: self.job_cost(&repository, &job).await?,
                    }
                }
                (QueueKind::Restore, Some(execution), Some(workspace)) if started_here => {
                    let work = self
                        .stopped_restore_work(&workspace, &execution, automatic)
                        .await?;
                    stopped.insert(execution);
                    work
                }
                (QueueKind::Restore, Some(execution), Some(workspace)) => {
                    let waits = waiters.contains(&execution);
                    let wake = self
                        .restore_wake(&workspace, &execution, automatic, waits)
                        .await?;
                    restores.insert(id.clone(), (execution, wake));
                    Work::Restore {
                        waiting: waits,
                        wake,
                    }
                }
                _ => Work::Launch,
            };
            waiting.push(Waiting {
                id,
                repository,
                work,
            });
        }
        let first_launch = waiting
            .iter()
            .position(|entry| entry.work == Work::Launch)
            .unwrap_or(waiting.len());
        let oneshots = self
            .agent_queue
            .waiting_oneshots()
            .into_iter()
            .map(|oneshot| Waiting {
                id: oneshot.id,
                repository: oneshot.repository,
                work: Work::OneShot {
                    after_stop: oneshot.after_stop,
                },
            });
        waiting.splice(first_launch..first_launch, oneshots);
        Ok(WaitingWork {
            entries: waiting,
            restores,
            stopped,
        })
    }

    /// Take the launches whose dependencies are not complete out of `waiting`,
    /// returning the branch each remaining launch with a done dependency
    /// builds on.
    async fn hold_back_dependents(
        &self,
        waiting: &mut Vec<Waiting>,
    ) -> Result<HashMap<String, String>> {
        let readiness = self.dependency_readiness().await?;
        waiting.retain(|entry| {
            !matches!(
                readiness.get(&entry.id),
                Some(dependencies::Readiness::Waiting(_))
            )
        });
        Ok(readiness
            .into_iter()
            .filter_map(|(id, readiness)| match readiness {
                dependencies::Readiness::Ready { base } => Some((id, base?)),
                dependencies::Readiness::Waiting(_) => None,
            })
            .collect())
    }

    /// Mark `admitted` entries starting, each on its base from `bases`,
    /// returning those the daemon starts: launches and the restores that
    /// carry a start environment.
    pub(super) async fn mark_starting(
        &self,
        admitted: Vec<String>,
        mut bases: HashMap<String, String>,
    ) -> Result<Vec<Start>> {
        self.store
            .write(move |tx| {
                let now = UnixTime::now();
                let mut starts = Vec::new();
                for id in admitted {
                    tx.execute(
                        "UPDATE agent_queue SET state=?2,seen_at=?3,base_branch=?4 WHERE id=?1",
                        params![id, QueueState::Starting, now, bases.remove(&id)],
                    )?;
                    let (name, env): (SpaceName, Option<String>) = tx.query_row(
                        "SELECT name,env FROM agent_queue WHERE id=?1",
                        [&id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    if let Some(env) = env {
                        starts.push(Start {
                            id,
                            name,
                            env: serde_json::from_str(&env).context("read queued environment")?,
                        });
                    }
                }
                Ok(starts)
            })
            .await
    }

    /// Start each admitted launch other than `caller`'s, which starts in the
    /// caller, and return whether `caller`'s was among them.
    pub(super) async fn start_launches(
        &self,
        starts: Vec<Start>,
        caller: Option<&str>,
    ) -> Result<bool> {
        let mut caller_admitted = false;
        for start in starts {
            if Some(start.id.as_str()) == caller {
                caller_admitted = true;
                continue;
            }
            match crate::daemon::queued_starts::spawn_start(&self.paths, &start) {
                Ok(()) => {
                    self.notify(
                        Some(&start.name),
                        NotificationKind::QueueStarted,
                        format!("Starting {} from the agent queue", start.name),
                    )
                    .await;
                }
                Err(error) => {
                    self.fail_queue_entry(&start.id, &format!("{error:#}"))
                        .await?
                }
            }
        }
        Ok(caller_admitted)
    }

    /// End starting launches and restores the daemon started whose holder has
    /// been gone for the grace period. A launch that never opened a workspace,
    /// like a restore whose agent never registered, failed to start, so say so.
    async fn expire_starting_entries(&self) -> Result<()> {
        let held = self.agent_queue.held();
        let cutoff = UnixTime::now() - START_GRACE;
        let expired = self
            .store
            .write(move |tx| expiry::expire_starts(tx, &held, cutoff))
            .await?;
        for failure in expired {
            self.report_expired_start(failure).await;
        }
        Ok(())
    }

    async fn report_expired_start(&self, failure: expiry::FailedStart) {
        let (name, reason) = match failure {
            expiry::FailedStart::UnopenedLaunch(name) => {
                (name, "its launch ended without opening the workspace")
            }
            expiry::FailedStart::UnstartedRestore(name) => {
                (name, "its restore ended before the agent started")
            }
        };
        self.notify(
            Some(&name),
            NotificationKind::QueueFailed,
            format!("{name} did not start from the agent queue: {reason}"),
        )
        .await;
    }

    /// Forget restores whose wrapper has been gone for the grace period, such
    /// as one that never reattached after a daemon restart. Its recovery
    /// record stays for `shoal resume`. A restore the daemon starts has no
    /// wrapper.
    async fn expire_orphaned_restores(&self) -> Result<()> {
        let connected = self.executions.connection_ids().await;
        let cutoff = UnixTime::now() - START_GRACE;
        self.store
            .write(move |tx| {
                let stale = tx
                    .prepare(
                        "SELECT execution_id FROM agent_queue
                         WHERE kind=?1 AND env IS NULL AND seen_at<?2",
                    )?
                    .query_map(params![QueueKind::Restore, cutoff], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<ExecutionId>>>()?;
                for execution in stale.iter().filter(|id| !connected.contains(*id)) {
                    tx.execute("DELETE FROM agent_queue WHERE execution_id=?1", [execution])?;
                }
                Ok(())
            })
            .await
    }

    /// Restart every starting launch's and restore's grace period, since no
    /// holder or wrapper could reconnect while the daemon was away.
    pub(crate) async fn refresh_starting_entries(&self) -> Result<()> {
        let now = UnixTime::now();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?3 WHERE state=?1 OR kind=?2",
                    params![QueueState::Starting, QueueKind::Restore, now],
                )?;
                Ok(())
            })
            .await
    }
}
